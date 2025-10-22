# 🧠 MindShard

MindShard is a shared memory of everything you browse. Built in Rust, it captures and indexes HTTP traffic with vector embeddings, enabling semantic search across your browsing history.

MindShard respects your privacy. All processing happens locally, and no third-party services are involved beyond the initial download of the embedding model from Hugging Face.

**This project is experimental and should be used for educational purposes only.** MindShard intercepts and stores HTTP traffic, which may include sensitive information. Use only in controlled environments. The author is not responsible for any misuse or data exposure resulting from the use of this software.

- [Why?](#why?)
- [Prerequisites](#prerequisites)
- [Build](#build)
- [Run](#run)
  - [Local](#local)
  - [Docker](#docker)
- [Configuration](#configuration)
- [Installing and Trusting the Root Certificate](#installing-and-trusting-the-root-certificate)
  - [MacOS](#macos)
  - [Windows](#windows)
  - [Linux (Ubuntu/Debian)](#linux-ubuntudebian)
- [Configuring Your System Proxy](#configuring-your-system-proxy)
  - [MacOS](#macos-1)
  - [Windows](#windows-1)
  - [Linux (Ubuntu/Debian)](#linux-ubuntudebian-1)
  - [Browser-Specific Configuration](#browser-specific-configuration)

## Why?

Have you ever read an interesting article, blog post, or research paper, only to struggle weeks later to recall the source of a quote or idea you remembered? You search through your browser history, try various keyword combinations, but can't locate the original source.

MindShard solves this problem by automatically capturing and indexing everything you read online using vector embeddings. It provides semantic search capabilities that let you find content based on meaning and context, not just exact keyword matches. Simply describe the idea or concept you remember, and MindShard will surface the relevant passages along with links to the original sources.

Think of it as a photographic memory for your web browsing - every article, blog post, and paper you read is preserved and instantly searchable by its actual content and meaning.

## Prerequisites

- Rust 1.88 or later
- OpenSSL

Generate a self-signed certificate and private key:

```bash
./create_cer.sh
```

This creates `mindshard.key` and `mindshard.cer` required for the proxy.

## Build

```bash
cargo build --release
```

## Run

### Local

```bash
cargo run --release
```

Make sure you have the required certificate files (`mindshard.key` and `mindshard.cer`) in your working directory.

### Docker

#### Quick Start with Docker Compose

The easiest way to run MindShard with Docker:

```bash
docker-compose up -d
```

This will:
- Build the Docker image
- Mount your certificates from the current directory
- Create a persistent volume for the database
- Start the application with ports 8080 (proxy) and 3000 (web) exposed

#### Building the image

```bash
docker build -t mindshard:latest .
```

#### Running with Docker

**Important:** The Docker container requires mounting the private key and certificate files.

Basic usage:
```bash
docker run -d \
  --name mindshard \
  -p 8080:8080 \
  -p 3000:3000 \
  -v $(pwd)/mindshard.key:/app/certs/mindshard.key:ro \
  -v $(pwd)/mindshard.cer:/app/certs/mindshard.cer:ro \
  -v mindshard-data:/app/data \
  mindshard:latest
```

#### Volume Mounts

**Required:**
- **Private Key**: `-v /path/to/mindshard.key:/app/certs/mindshard.key:ro`
- **Certificate**: `-v /path/to/mindshard.cer:/app/certs/mindshard.cer:ro`

**Recommended:**
- **Database** (for persistence): `-v mindshard-data:/app/data` or `-v $(pwd)/data:/app/data`

## Configuration

The application is configured via environment variables:

| Variable                     | Default         | Description                      |
|------------------------------|-----------------|----------------------------------|
| `MINDSHARD_PROXY_PORT`       | `8080`          | Port for the HTTP proxy server   |
| `MINDSHARD_WEB_PORT`         | `3000`          | Port for the web server          |
| `MINDSHARD_PRIVATE_KEY_PATH` | `mindshard.key` | Path to the private key file     |
| `MINDSHARD_CA_CERT_PATH`     | `mindshard.cer` | Path to the CA certificate file  |
| `MINDSHARD_DB_PATH`          | `mindshard.db`  | Path to the LibSQL database file |

## Installing and Trusting the Root Certificate

To avoid browser security warnings when using the proxy, you need to install and trust the generated `mindshard.cer` certificate as a trusted root certificate authority on your system.

### MacOS

1. Double-click `mindshard.cer` to open Keychain Access
2. Select "System" keychain and click "Add"
3. Find the certificate in Keychain Access, double-click it
4. Expand "Trust" section and set "When using this certificate" to "Always Trust"
5. Close the window and enter your password to confirm

[Official guide](https://support.apple.com/guide/keychain-access/add-certificates-to-a-keychain-kyca2431/mac)

### Windows

1. Double-click `mindshard.cer`
2. Click "Install Certificate"
3. Select "Local Machine" and click "Next"
4. Select "Place all certificates in the following store"
5. Click "Browse" and select "Trusted Root Certification Authorities"
6. Click "Next" and then "Finish"

[Official guide](https://learn.microsoft.com/en-us/skype-sdk/sdn/articles/installing-the-trusted-root-certificate)

### Linux (Ubuntu/Debian)

```bash
sudo cp mindshard.cer /usr/local/share/ca-certificates/mindshard.crt
sudo update-ca-certificates
```

For Firefox on Linux, you need to import the certificate separately in Firefox settings under Privacy & Security > Certificates > View Certificates > Authorities > Import.

[Official guide](https://ubuntu.com/server/docs/install-a-root-ca-certificate-in-the-trust-store)

## Configuring Your System Proxy

To route traffic through MindShard, configure your system's HTTP(S) proxy settings to point to `localhost:8080` (or your configured `MINDSHARD_PROXY_PORT`).

### MacOS

1. Open **System Settings** > **Network**
2. Select your active network connection (Wi-Fi or Ethernet)
3. Click **Details** > **Proxies**
4. Check **Web Proxy (HTTP)** and **Secure Web Proxy (HTTPS)**
5. For both, enter:
   - **Server**: `localhost` or `127.0.0.1`
   - **Port**: `8080`
6. Click **OK** and **Apply**

### Windows

1. Open **Settings** > **Network & Internet** > **Proxy**
2. Under **Manual proxy setup**, toggle **Use a proxy server** to **On**
3. Enter:
   - **Address**: `localhost` or `127.0.0.1`
   - **Port**: `8080`
4. Click **Save**

Alternatively, via Command Prompt (as Administrator):
```cmd
netsh winhttp set proxy localhost:8080
```

To disable:
```cmd
netsh winhttp reset proxy
```

### Linux (Ubuntu/Debian)

**GNOME Desktop:**
1. Open **Settings** > **Network** > **Network Proxy**
2. Select **Manual**
3. For **HTTP Proxy** and **HTTPS Proxy**, enter:
   - **Host**: `localhost` or `127.0.0.1`
   - **Port**: `8080`
4. Click **Apply**

**Command Line (for current session):**
```bash
export http_proxy=http://localhost:8080
export https_proxy=http://localhost:8080
```

**Persistent (add to `~/.bashrc` or `~/.zshrc`):**
```bash
echo 'export http_proxy=http://localhost:8080' >> ~/.bashrc
echo 'export https_proxy=http://localhost:8080' >> ~/.bashrc
source ~/.bashrc
```

### Browser-Specific Configuration

Some browsers (like Firefox) use their own proxy settings independent of system settings:

**Firefox:**
1. Open **Settings** > **General** > **Network Settings**
2. Click **Settings**
3. Select **Manual proxy configuration**
4. For **HTTP Proxy** and **HTTPS Proxy**, enter:
   - **Host**: `localhost`
   - **Port**: `8080`
5. Check **Also use this proxy for HTTPS**
6. Click **OK**
