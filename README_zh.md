# USB Resolver & Device Monitor

**专为 Rust 设计的跨平台 USB 设备映射与热插拔监控库。**

[English](README.md) | 简体中文

## 简介

这是一个专为机器人和嵌入式系统设计的跨平台 Rust 库。它解决了 **“如何在多变的 USB 端口号中稳定找到特定硬件”** 这一痛点。

通过配置逻辑角色（如 `top_camera`），库会自动扫描系统，根据 VID/PID、序列号或物理端口路径将设备匹配到角色，并在设备热插拔时发出事件通知。

## 🎯 核心特性

* **跨平台统一抽象**：在 Linux、macOS、Windows 上提供完全一致的 API。
* **逻辑角色绑定**：不再硬编码 `/dev/ttyUSB0`，而是使用 `left_arm_sensor`。
* **高可靠热插拔**：实时监听插拔事件，内置断线重连和防抖逻辑。
* **智能匹配策略**：支持 **序列号**（最推荐）、**物理端口路径**（无序列号时使用）或 **VID/PID**。
* **设备树回溯 (Linux)**：自动处理 USB Interface 事件，向上查找父级 USB Device，解决部分设备属性读取为空的问题。

## 🏗 系统架构与实现细节

### 1. Linux (`udev`)

* **实现机制**：基于 `udev` Netlink Socket。
* **健壮性设计**：
* 内置 **Keep-Alive 循环**：如果 Socket 因权限或环境问题断开，会自动尝试重建连接，确保监听线程不退出。
* **父节点查找**：当内核发送 `Interface`（接口）事件时，自动回溯查找父级 `Device`，确保能读取到 `idVendor` 等关键信息。
* **精准移除**：使用 `syspath` 作为唯一标识处理移除事件，避免因文件已删除导致的读取错误。


* **路径处理**：自动关联设备下的 `tty` 节点（如 `/dev/ttyUSB0`）。

### 2. macOS (`IOKit`)

* **实现机制**：基于 `IOKit` 框架。
* **竞态处理**：针对设备插入后驱动加载的延迟，内置了退避重试机制。
* **双路径支持**：提供主路径 `/dev/cu.xxx` (Callout) 和备用路径 `/dev/tty.xxx` (Dialin)。

### 3. Windows (`SetupAPI`)

* **实现机制**：基于 `SetupAPI` 和配置管理器。
* **策略**：采用 **1秒间隔轮询 (Polling)** 机制，在工业场景下比 Windows 复杂的窗口消息回调更稳定。

## 🚀 快速开始

### 1. 环境准备

**Linux (Ubuntu/Debian):**

```bash
sudo apt update && sudo apt install libudev-dev pkg-config build-essential
```

**Linux (Fedora/CentOS):**

```bash
sudo dnf install systemd-devel
```

### 2. 扫描设备信息 (Discovery)

在编写配置前，请先运行工具获取设备的真实硬件信息。

```bash
# Linux 下建议使用 sudo 以获取完整属性
cargo run --example discovery
```

**输出示例：**

| VID(Hex) | VID(Dec) | PID(Hex) | PID(Dec) | Serial | Port Path | Path |
|---|---|---|---|---|---|---|
|0x1d6b     | 7531       | 0x0002     | 2          | 0000:17:00.0         | pci-0000:17:00.0          | /dev/bus/usb/001/001|
|0x1d6b     | 7531       | 0x0003     | 3          | 0000:17:00.0         | pci-0000:17:00.0          | /dev/bus/usb/002/001|
|0x05e3     | 1507       | 0x0626     | 1574       | N/A                  | pci-0000:17:00.0-usb-0:1  | /dev/bus/usb/002/002|
|0x1d6b     | 7531       | 0x0002     | 2          | 0000:80:14.0         | pci-0000:80:14.0          | /dev/ttyACM0|
|0x8087     | 32903      | 0x0036     | 54         | N/A                  | pci-0000:80:14.0-usb-0:14 | /dev/bus/usb/003/004|
|0x5986     | 22918      | 0x1193     | 4499       | 0001                 | pci-0000:80:14.0-usb-0:4  | /dev/bus/usb/003/002|
|0x05e3     | 1507       | 0x0610     | 1552       | N/A                  | pci-0000:80:14.0-usb-0:6  | /dev/ttyACM0|
|0x1a86     | 6790       | 0x55d3     | 21971      | 5AB0183575           | pci-0000:80:14.0-usb-0:6.3 | /dev/ttyACM0|
|0x046d     | 1133       | 0x082d     | 2093       | 91D1D1FF             | pci-0000:80:14.0-usb-0:6.4 | /dev/bus/usb/003/008|
|0x1462     | 5218       | 0x1603     | 5635       | 25D220FA0000         | pci-0000:80:14.0-usb-0:9  | /dev/bus/usb/003/003|
|0x1d6b     | 7531       | 0x0003     | 3          | 0000:80:14.0         | pci-0000:80:14.0          | /dev/bus/usb/004/001|

### 3. 配置文件 (`device_config.json`)

```json
[
  {
    "role": "imu_sensor", // 我们配置的名称，该字段在整个文件中保持唯一
    "vid": 9025,
    "pid": 32822,
    "serial": "SN-12345",
    "port_path": null
  },
  {
    "role": "led_controller",
    "vid": 6790,
    "pid": 29987,
    "serial": null,
    "port_path": "1-2.2"
  }
]
```

### 4. 代码集成

```rust
use usb_resolver::{get_monitor, DeviceRule, DeviceEvent};
use std::fs;

fn main() -> anyhow::Result<()> {
    // 1. 读取配置
    let config = fs::read_to_string("device_config.json")?;
    let rules: Vec<DeviceRule> = serde_json::from_str(&config)?;

    // 2. 获取实例与通道
    let monitor = get_monitor();
    let (tx, rx) = crossbeam_channel::unbounded();

    // 3. 启动后台监听
    monitor.start(rules, tx)?;
    println!("服务已启动...");

    // 4. 事件处理循环 (注意：主线程不能退出)
    for event in rx {
        match event {
            DeviceEvent::Attached(dev) => {
                println!("✅ 设备上线: {}", dev.role);
                // 根据平台选择最佳打开路径
                let port = if cfg!(target_os = "windows") {
                    dev.device.system_path_alt.as_deref().unwrap_or(&dev.device.system_path)
                } else {
                    &dev.device.system_path
                };
                println!("   -> 端口路径: {}", port);
            },
            DeviceEvent::Detached(role) => {
                println!("❌ 设备下线: {}", role);
            }
        }
    }
    Ok(())
}
```

## 🛠 常见问题排查 (Troubleshooting)

1. **Linux 下没有检测到设备？**
* **权限问题**：udev 需要权限。请尝试 `sudo cargo run`。
* **规则文件**：如果不想用 root 运行，请配置 `/etc/udev/rules.d/` 允许当前用户访问 USB 设备。


2. **程序刚启动就退出了？**
* Rust 的主线程一旦结束，后台监听线程也会被杀死。请确保主线程里有一个 `loop` 或 `rx.recv()` 阻塞操作。


3. **`port_path` 不匹配？**
* 物理端口路径与主板 USB 拓扑有关。如果你把设备换了一个 USB 口，`port_path` 会改变。请重新运行 discovery 工具查看。


4. **TUI 界面中无法看到调试日志？**
* TUI 会接管标准输出。建议配置 `log` 库将日志写入文件（`WriteLogger`），然后使用 `tail -f debug.log` 查看。
