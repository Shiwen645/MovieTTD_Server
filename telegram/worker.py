"""Private JSON-lines worker. Credentials/session arrive over stdin, never argv or disk."""
import asyncio
import json
import logging
import sys
from urllib.parse import urlsplit, unquote

logging.disable(logging.CRITICAL)
client = None
phone = ""
phone_hash = ""
watched = set()


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)

def message_payload(message, channel_id=None):
    text = getattr(message, "raw_text", "") or ""
    # Hidden URL entities are not present in raw_text.
    for entity in getattr(message, "entities", None) or []:
        url = getattr(entity, "url", None)
        if url:
            text += "\n" + url
    return {"channelId": str(channel_id or message.chat_id), "messageId": message.id,
            "text": text, "date": int(message.date.timestamp()),
            "editDate": int(message.edit_date.timestamp()) if message.edit_date else 0}

async def on_message(event):
    if str(event.chat_id) in watched:
        emit({"event": "message", "data": message_payload(event.message)})


def checkpoint_session(value):
    # Telethon 1.45 StringSession only saves the auth key. Preserve update cursors
    # and entity access hashes too, in the Rust encrypted vault. No .session file.
    from datetime import datetime, timezone
    from telethon.sessions import StringSession
    from telethon.tl.types import updates
    class VaultSession(StringSession):
        def __init__(self, raw):
            payload = json.loads(raw) if raw.startswith("{") else {"auth": raw}
            super().__init__(payload.get("auth", ""))
            # MemorySession's entity rows are pinned to Telethon 1.45.0; tested below.
            self._entities.update(tuple(row) for row in payload.get("entities", []))
            for row in payload.get("states", []):
                entity, pts, qts, date, seq = row
                self.set_update_state(entity, updates.State(pts, qts, datetime.fromtimestamp(date, timezone.utc), seq, 0))
            self.previous_states = payload.get("states", [])
        def save(self):
            states = [[entity, state.pts, state.qts, int(state.date.timestamp()), state.seq]
                      for entity, state in self.get_update_states()]
            # Keep one checkpoint behind so an unhandled in-flight update is replayed.
            snapshot = json.dumps({"auth": StringSession.save(self), "states": self.previous_states,
                                   "entities": [[row[0], row[1], row[2], None, None] for row in self._entities]}, ensure_ascii=False)
            self.previous_states = states
            if self.auth_key:
                emit({"event": "session", "session": snapshot})
            return snapshot
    return VaultSession(value)

async def resolve(value):
    value = str(value).strip()
    if value.lstrip("-").isdigit():
        value = int(value)
    return await client.get_entity(value)

async def command(data):
    global client, phone, phone_hash, watched
    from telethon import TelegramClient, events, utils
    from telethon.sessions import StringSession
    op = data["op"]
    if op == "init":
        if client:
            await client.disconnect()
        watched = set(data.get("watched", []))
        options = {}
        proxy = data.get("proxy", "")
        if proxy:
            from python_socks import ProxyType
            parsed = urlsplit(proxy)
            kind = {"socks5": ProxyType.SOCKS5, "socks4": ProxyType.SOCKS4, "http": ProxyType.HTTP}[parsed.scheme]
            options["proxy"] = {"proxy_type": kind, "addr": parsed.hostname, "port": parsed.port,
                                "username": unquote(parsed.username or ""), "password": unquote(parsed.password or ""), "rdns": True}
        client = TelegramClient(checkpoint_session(data.get("session", "")), int(data["apiId"]), data["apiHash"],
                                sequential_updates=True, catch_up=True,
                                flood_sleep_threshold=0, request_retries=1, connection_retries=3,
                                retry_delay=2, timeout=15, **options)
        client.add_event_handler(on_message, events.NewMessage())
        client.add_event_handler(on_message, events.MessageEdited())
        await client.connect()
        authorized = await client.is_user_authorized()
        me = await client.get_me() if authorized else None
        return {"authorized": authorized, "name": ((me.first_name or "") if me else ""), "session": client.session.save() if authorized else ""}
    if not client:
        raise ValueError("请先保存 Telegram API 配置")
    if op == "status":
        return {"authorized": await client.is_user_authorized(), "connected": client.is_connected()}
    if op == "send_code":
        phone = data["phone"]
        result = await client.send_code_request(phone)
        phone_hash = result.phone_code_hash
        return {"stage": "code", "message": "验证码已请求，请检查 Telegram 或短信"}
    if op == "sign_in":
        from telethon.errors import SessionPasswordNeededError
        try:
            await client.sign_in(phone=phone, code=data["code"], phone_code_hash=phone_hash)
        except SessionPasswordNeededError:
            return {"stage": "password", "message": "请输入 Telegram 两步验证密码"}
        return {"stage": "authorized", "session": client.session.save()}
    if op == "password":
        await client.sign_in(password=data["password"])
        return {"stage": "authorized", "session": client.session.save()}
    if op == "logout":
        await client.log_out()
        return {"stage": "logged_out"}
    if op == "resolve":
        entity = await resolve(data["channel"])
        if not getattr(entity, "broadcast", False) and not getattr(entity, "megagroup", False):
            raise ValueError("请选择有权访问的频道或超级群组")
        messages = await client.get_messages(entity, limit=1)
        return {"channelId": str(utils.get_peer_id(entity)), "title": entity.title,
                "username": getattr(entity, "username", "") or "", "latestId": messages[0].id if messages else 0}
    if op == "watch":
        # Resolving warms Telethon's access-hash cache. No joining or posting occurs.
        peers = set()
        for channel in data["channels"]:
            entity = await resolve(channel)
            peers.add(str(utils.get_peer_id(entity)))
        watched = peers
        return {"count": len(watched)}
    if op == "history":
        entity = await resolve(data["channel"])
        limit = min(int(data.get("limit", 30)), 100)
        kwargs = {"limit": limit, "min_id": int(data.get("minId", 0))}
        if data.get("forward"):
            kwargs["reverse"] = True
        else:
            kwargs["max_id"] = int(data.get("maxId", 0))
        messages = await client.get_messages(entity, **kwargs)
        return {"messages": [message_payload(m, utils.get_peer_id(entity)) for m in messages], "done": len(messages) < limit}
    raise ValueError("不支持的 Telegram 操作")

async def main():
    try:
        import telethon
    except ImportError:
        emit({"event": "fatal", "message": "服务器缺少 Telethon，请安装 server/telegram/requirements.txt"})
        return
    while True:
        line = await asyncio.to_thread(sys.stdin.readline)
        if not line:
            break
        try:
            data = json.loads(line)
            result = await command(data)
            emit({"id": data["id"], "ok": True, "data": result})
        except Exception as error:
            from telethon.errors import FloodWaitError
            seconds = error.seconds if isinstance(error, FloodWaitError) else 0
            # Never echo exception text; it may contain phone numbers or access hashes.
            names = {"PhoneCodeInvalidError": "验证码错误", "PhoneCodeExpiredError": "验证码已过期，请重新获取",
                     "PasswordHashInvalidError": "两步验证密码错误", "PhoneNumberInvalidError": "手机号格式错误",
                     "ApiIdInvalidError": "API_ID 或 API_HASH 无效", "ChannelPrivateError": "当前账号无权访问该频道",
                     "UsernameNotOccupiedError": "频道不存在", "AuthKeyUnregisteredError": "Telegram 登录失效，请重新登录"}
            message = names.get(type(error).__name__, "Telegram 操作失败，请检查账号权限、网络和代理设置")
            if isinstance(error, ValueError) and str(error).startswith(("请选择", "请先", "不支持")):
                message = str(error)
            if seconds:
                message = f"Telegram 要求等待 {seconds} 秒，任务已暂停"
            emit({"id": data.get("id", ""), "ok": False, "error": message, "retryAfter": seconds})
    if client:
        await client.disconnect()

if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
