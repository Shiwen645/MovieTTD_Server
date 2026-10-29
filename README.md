# MovieTTD 后端部署指南<div align="center">

<img src="./icon.png" width="120" alt="MovieTTD" />

# MovieTTD · Server

**MovieTTD 的后端服务 —— 为影视媒体库提供统一、容器化的后端能力**

基于 **Rust** 构建核心服务，集成 **Python** Telegram 机器人组件，通过 **Docker Compose** 一键部署。支持媒体文件管理、健康检查、初始管理员配置，以及 Telegram 机器人后台任务。

[![Release](https://img.shields.io/github/v/release/Shiwen645/MovieTTD_Server?style=flat-square&label=release&color=4C8BF5)](https://github.com/Shiwen645/MovieTTD_Server/releases)
[![Stars](https://img.shields.io/github/stars/Shiwen645/MovieTTD_Server?style=flat-square&logo=github&color=4C8BF5)](https://github.com/Shiwen645/MovieTTD_Server/stargazers)
[![License](https://img.shields.io/badge/license-AGPL--3.0-blue?style=flat-square)](./LICENSE)
[![Rust](https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3776AB?style=flat-square&logo=python&logoColor=white)](https://www.python.org/)
[![Docker](https://img.shields.io/badge/Docker-2496ED?style=flat-square&logo=docker&logoColor=white)](https://www.docker.com/)

[部署指南](#部署) · [环境要求](#环境要求) · [快速启动](#快速启动) · [常见问题](#常见问题)

</div>

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
