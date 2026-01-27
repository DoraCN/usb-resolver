# USB Resolver & Device Monitor

这是一个专为机器人和嵌入式系统设计的跨平台 Rust 库。它解决了**“如何在多变的 USB 端口号中稳定找到特定硬件”**这一痛点。

通过配置逻辑角色（如 `top_camera`），库会自动扫描系统，根据 VID/PID、序列号或物理端口路径将设备匹配到角色，并在设备热插拔时发出事件通知。

## 🎯 核心特性

* **跨平台统一抽象**：在 Linux、macOS、Windows 上提供完全一致的 API。
* **逻辑角色绑定**：不再硬编码 `/dev/ttyUSB0`，而是使用 `left_arm_sensor`。
* **热插拔监听**：设备插入/拔出时实时通知，支持断线重连逻辑。
* **多重匹配策略**：支持按 **序列号 (Serial)**（最推荐）、**物理端口路径 (Port Path)**（无序列号时使用）或 **VID/PID** 匹配。

---

## 🏗 操作系统架构差异与统一 API

尽管对外暴露的 API 是统一的，但为了保证稳定性，底层针对不同 OS 采用了不同的实现策略：

### 1. Linux

* **实现机制**：基于 `udev`。
* **监听模式**：
* 优先尝试 **Netlink Socket** 监听内核 Uevent（低延迟，高性能）。
* 如果环境不支持（如 WSL2 或 Docker 容器），自动降级为 **Polling（轮询）** 模式（2秒间隔）。


* **路径处理**：自动查找 USB 设备下属的 `tty` 节点（如 `/dev/ttyUSB0` 或 `/dev/ttyACM0`）作为主路径。
* **依赖**：需要系统安装 `libudev-dev`。

### 2. macOS

* **实现机制**：基于 `IOKit` 框架。
* **竞态处理**：解决了设备插入后驱动加载延迟导致的 Race Condition。带有自动退避重试机制，确保能读到 `/dev` 节点。
* **双路径支持**：
* **主路径 (system_path)**：`/dev/cu.xxx` (Callout Device) —— **推荐使用**，Open 时不会阻塞等待 DCD 信号。
* **备用路径 (system_path_alt)**：`/dev/tty.xxx` (Dialin Device)。



### 3. Windows

* **实现机制**：基于 `SetupAPI` 和 `Configuration Manager`。
* **策略**：采用 **1秒间隔的轮询 (Polling)** 机制。这是因为 Windows 的回调机制在非 GUI 程序的子线程中极不稳定，轮询方案在工业场景下最为健壮。
* **路径处理**：
* **主路径**：设备实例 ID (Instance ID)，如 `USB\VID_xxxx&PID_xxxx\SN...`。
* **备用路径**：自动提取 COM 口号 (如 `COM3`)，方便串口库调用。



---

## 🚀 快速开始

### 1. 环境准备

**Linux (Ubuntu/Debian):**

```bash
sudo apt update
sudo apt install libudev-dev pkg-config build-essential

```

**macOS / Windows:**
无需额外安装，只需安装 Rust 工具链。

### 2. 扫描设备 (Discovery)

在编写配置文件前，你需要知道设备的真实信息（VID, PID, 序列号, 物理路径）。我们提供了一个工具来获取这些信息。

运行以下命令：

```bash
# Linux 下可能需要 sudo 权限以读取完整信息
cargo run --example discovery

```

**输出示例：**

```text
VID(Hex) | VID(Dec) | PID(Hex) | PID(Dec) | Serial       | Port Path  | Path
-----------------------------------------------------------------------------------------------
0x2341   | 9025     | 0x8036   | 32822    | SN-12345     | 1-2.1      | /dev/ttyUSB0  /  ...
0x1a86   | 6790     | 0x7523   | 29987    | N/A          | 0x14200000 | /dev/cu.usb... / ...

```

* **VID(Dec) / PID(Dec)**：请复制**十进制**数值到配置文件中。
* **Serial**：如果有值，优先使用序列号匹配。
* **Port Path**：如果序列号是 `N/A`（通常是廉价芯片），则需要使用物理端口路径进行绑定。

### 3. 创建配置文件 (`device_config.json`)

在项目根目录下创建 `device_config.json`。

**字段详细说明：**

| 字段名      | 类型    | 必填 | 说明                                                                                                                                            |
| ----------- | ------- | ---- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `role`      | String  | ✅    | **逻辑角色名**。这是你在代码中使用的唯一标识（例如 `"lidar_main"`）。                                                                           |
| `vid`       | Integer | ✅    | **厂商 ID (十进制)**。从 `discovery` 工具的 VID(Dec) 列获取。                                                                                   |
| `pid`       | Integer | ✅    | **产品 ID (十进制)**。从 `discovery` 工具的 PID(Dec) 列获取。                                                                                   |
| `serial`    | String  | ❌    | **设备序列号**。这是**最优先**的匹配条件。如果设备有序列号，请务必填入。                                                                        |
| `port_path` | String  | ❌    | **物理端口路径**。只有当设备没有序列号，且你需要区分两个相同的设备（插在不同USB口）时才使用。格式因 OS 而异（Linux: `1-2`, macOS: `0x14...`）。 |

**配置示例：**

```json
[
  {
    "role": "top_camera",
    "vid": 12944,
    "pid": 9797,
    "serial": "SN-8899-CAM",
    "port_path": null
  },
  {
    "role": "left_arm_sensor",
    "vid": 6790,
    "pid": 29987,
    "serial": null,
    "port_path": "1-2.1"
  }
]

```

*(注意：`port_path` 是平台相关的，如果在不同电脑或不同 OS 间迁移，通常需要修改该字段)*

---

## 📚 统一 API 使用指南

### 核心结构体

**1. `ResolvedDevice` (事件中返回的对象)**

```rust
pub struct ResolvedDevice {
    pub role: String,           // 配置文件中定义的角色名 (如 "top_camera")
    pub device: RawDeviceInfo,  // 设备的底层物理信息
    pub match_method: MatchMethod, // 它是通过什么规则匹配上的 (Serial, Port, etc.)
}

```

**2. `RawDeviceInfo` (设备物理信息)**

```rust
pub struct RawDeviceInfo {
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    pub port_path: String,

    // 核心路径字段
    pub system_path: String,      // 主路径。Linux/macOS 下通常是设备节点，Windows 下是 InstanceID
    pub system_path_alt: Option<String>, // 备用路径。macOS 下是 /dev/tty.*，Windows 下是 COMx
}

```

### 代码集成示例

```rust
use usb_resolver::{get_monitor, DeviceRule, DeviceEvent};
use std::fs;

fn main() -> anyhow::Result<()> {
    // 1. 读取配置
    let config_content = fs::read_to_string("device_config.json")?;
    let rules: Vec<DeviceRule> = serde_json::from_str(&config_content)?;

    // 2. 获取监控器实例 (自动适配当前 OS)
    let monitor = get_monitor();

    // 3. 创建通信通道
    let (tx, rx) = crossbeam_channel::unbounded();

    // 4. 启动后台监控 (非阻塞)
    monitor.start(rules, tx)?;

    println!("服务已启动，等待设备...");

    // 5. 处理事件循环
    loop {
        match rx.recv() {
            Ok(event) => match event {
                DeviceEvent::Attached(resolved) => {
                    println!("设备上线: {}", resolved.role);

                    // 获取用于打开串口的路径
                    // macOS 优先使用 system_path (/dev/cu.*)
                    // Windows 优先使用 system_path_alt (COMx)
                    let path_to_open = if cfg!(target_os = "windows") {
                        resolved.device.system_path_alt.as_ref().unwrap_or(&resolved.device.system_path)
                    } else {
                        &resolved.device.system_path
                    };

                    println!("应打开端口: {}", path_to_open);
                    // TODO: serialport::new(path_to_open, 115200).open()...
                },
                DeviceEvent::Detached(role) => {
                    println!("设备下线: {}", role);
                    // TODO: 停止相关业务线程
                },
            },
            Err(_) => break, // 通道关闭或错误
        }
    }
    Ok(())
}

```

---

## 🛠 常见问题排查

1. **Linux 下 `cargo run` 直接退出？**
* 原因：权限不足导致 udev socket 创建失败。
* 解决：使用 `sudo cargo run`，或配置 udev rules 允许非 root 用户访问。


2. **macOS 下找不到 `/dev/cu.*`？**
* 原因：驱动加载延迟。
* 解决：库内部已包含重试机制。如果依然失败，请检查 USB 线是否松动。


3. **Windows 下看不到 COM 口？**
* 原因：有些纯 USB 设备（如键盘鼠标）没有 COM 口，只有 Instance ID。
* 解决：`discovery` 工具会显示 `system_path_alt` 为空。只有串口类设备才有 COM 口。


4. **`port_path` 怎么填？**
* 一定要使用 `cargo run --example discovery` 在目标机器上实际运行一次，不同主板、不同集线器的路径都不同。不要凭感觉猜测。