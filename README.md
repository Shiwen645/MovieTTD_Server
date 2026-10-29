<div align="center">

<img src="https://p3-luna.douyinpic.com/img/tos-cn-i-b829550vbb/625c3e93e6d541a7b2709982e24139f1~tplv-b829550vbb-image.jpeg" width="120" style="border-radius: 15px;" alt="MovieTTD" />

# MovieTTD · Server

**MovieTTD 的后端服务 —— 为影视媒体库提供统一、容器化的后端能力**

基于 **Rust** 构建核心服务，集成 **Python** Telegram 机器人组件，通过 **Docker Compose** 一键部署。支持媒体文件管理、健康检查、初始管理员配置，以及 Telegram 后台任务。

> 本项目基于 [YunX](https://github.com/CYQawa/YunX) 二次开发，遵循 **GNU AGPL-3.0** 协议开源。

[![Release](https://img.shields.io/github/v/release/Shiwen645/MovieTTD_Server?style=flat-square&label=release&color=4C8BF5)](https://github.com/Shiwen645/MovieTTD_Server/releases)
[![Stars](https://img.shields.io/github/stars/Shiwen645/MovieTTD_Server?style=flat-square&logo=github&color=4C8BF5)](https://github.com/Shiwen645/MovieTTD_Server/stargazers)
[![License](https://img.shields.io/badge/license-AGPL--3.0-blue?style=flat-square)](./LICENSE)
[![Rust](https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3776AB?style=flat-square&logo=python&logoColor=white)](https://www.python.org/)
[![Docker](https://img.shields.io/badge/Docker-2496ED?style=flat-square&logo=docker&logoColor=white)](https://www.docker.com/)

[部署指南](#部署) · [环境要求](#环境要求) · [快速启动](#快速启动) · [协议与致谢](#协议与致谢) · [常见问题](#常见问题)

</div>

# MovieTTD 后端部署指南

### 1. 需要哪些环境？

只需要这两个：

| 名称 | 是什么 |
|---|---|
| Docker | 在服务器上运行后端容器的工具 |
| Docker Compose | 按配置文件构建、启动和管理容器的工具 |

Compose 就是读取项目里的 `docker-compose.yml`，完成构建和启动。

在 **1Panel → 终端**运行：

```bash
docker version
docker compose version
```

第一条能看到 **Server** 信息，第二条显示版本号，就说明环境已有，

### 2. 上传哪些文件？

在 1Panel 的文件管理中，建立：

```text
/opt/moviettd/server
```

**只上传下面这些内容：**

```text
/opt/moviettd/server/
├── src/                    上传整个目录，覆盖旧文件
├── telegram/
│   ├── worker.py
│   └── requirements.txt
├── Cargo.toml
├── Cargo.lock
├── Dockerfile
└── .dockerignore            隐藏文件，也要上传
```

### 3. 修改服务器 IP

在 1Panel 中编辑刚上传的 `docker-compose.yml`，找到：

```yaml
MOVIETTD_PUBLIC_URL: "http://YOUR_SERVER_IP:18766"
```

把 `YOUR_SERVER_IP` 换成你的服务器公网 IP，然后保存。

### 4. 创建数据保存目录

在 **1Panel 主机终端，以 root 身份**执行：

```bash
mkdir -p /opt/moviettd/data /opt/moviettd/media
chown 10001:10001 /opt/moviettd/data /opt/moviettd/media
```

这里：

- `data` 保存账号、凭证、配置和任务。
- `media` 保存 STRM、NFO 和图片。
- `chown` 是给容器里的程序写入权限，`10001` 是配置好的服务账号编号。

### 5. 启动后端

仍在 1Panel 终端执行：

```bash
docker compose -p moviettd -f /opt/moviettd/server/docker-compose.yml up -d --build
```

它会自动下载构建环境、编译后端并在后台运行。第一次需要等待，**构建镜像和缓存仍可能占用较多磁盘**，但不会把你本地的两三 GB 缓存上传过去。

配置中已设置自动重启策略：Docker 正常开机启动时，后端也会自动启动；手动停止的容器会保持停止。citeturn296491search1

### 6. 放行端口并连接

在 **1Panel 防火墙（如果启用）和云服务器安全组**中，放行 TCP `18766`。

浏览器访问：

```text
http://你的服务器IP:18766/health
```

返回正常 JSON 后，在本地软件填写：

```text
http://你的服务器IP:18766
```

首次账号密码可以在 1Panel 中查看这个文件：

```text
/opt/moviettd/data/initial-admin.json
```

软件里的 `/media` 保存目录，对应服务器的：

```text
/opt/moviettd/media
```


## 协议与致谢

本项目（MovieTTD_Server）基于 [YunX](https://github.com/CYQawa/YunX) 二次开发，
遵循 **GNU Affero General Public License v3.0（AGPL-3.0）** 协议开源。

- **原项目**：[YunX](https://github.com/CYQawa/YunX) · 作者 [CYQawa](https://github.com/CYQawa) · 协议 AGPL-3.0
- **本项目**：MovieTTD_Server · 在 YunX 基础上进行了服务端化、Docker 化等修改
- **协议全文**：见仓库根目录 [LICENSE](./LICENSE)

根据 AGPL-3.0 协议要求，任何基于本项目的修改与分发都必须：

1. 保留原作者的版权声明与协议声明；
2. 继续以 AGPL-3.0 协议开源；
3. 向通过网络使用本服务的用户提供完整源代码的获取方式。

感谢 YunX 作者 CYQawa 的开源贡献 🙏
