# USB Resolver & Device Monitor

**A cross-platform USB device mapping and hot-plug monitoring library designed for Rust.**

English | [简体中文](README_zh.md)

## Introduction

This is a cross-platform Rust library designed for robotics and embedded systems. It solves the problem of **"how to reliably find specific hardware among varying USB port numbers."**

By configuring logical roles (such as `top_camera`), the library automatically scans the system, matches devices to roles based on VID/PID, serial number, or physical port path, and sends event notifications when devices are hot-plugged or unplugged.

## 🎯 Core Features

* **Cross-platform unified abstraction:** Provides a completely consistent API on Linux, macOS, and Windows.
* **Logical role binding:** No more hardcoding `/dev/ttyUSB0`, instead use `left_arm_sensor`.
* **Highly reliable hot-plugging:** Real-time monitoring of plug and unplug events, with built-in reconnection and debouncing logic.
* **Intelligent matching strategy:** Supports **serial number** (most recommended), **physical port path** (used when no serial number is available), or **VID/PID**.
* **Device tree backtracking (Linux):** Automatically handles USB Interface events, tracing back to the parent USB Device to resolve issues where some device attributes are empty.

## 🏗 System Architecture and Implementation Details

### 1. Linux (`udev`)

* **Implementation Mechanism:** Based on `udev` Netlink Socket.
* **Robustness Design:**
* Built-in **Keep-Alive loop:** If the Socket disconnects due to permissions or environmental issues, it will automatically attempt to rebuild the connection, ensuring the listening thread does not exit.
* **Parent node lookup:** When the kernel sends an `Interface` event, it automatically traces back to find the parent `Device` to ensure that key information such as `idVendor` can be read.
* **Precise removal:** Uses `syspath` as a unique identifier to handle removal events, avoiding read errors caused by deleted files.

* **Path handling:** Automatically associates the `tty` node under the device (e.g., `/dev/ttyUSB0`).

### 2. macOS (`IOKit`)

* **Implementation Mechanism:** Based on the `IOKit` framework. * **Race Condition Handling:** A built-in backoff and retry mechanism is included to address delays in driver loading after device insertion.
* **Dual Path Support:** Provides a primary path `/dev/cu.xxx` (Callout) and an alternative path `/dev/tty.xxx` (Dialin).

### 3. Windows (`SetupAPI`)

* **Implementation Mechanism:** Based on `SetupAPI` and the Configuration Manager.
* **Strategy:** Uses a **1-second interval polling** mechanism, which is more stable in industrial scenarios than the complex window message callbacks used in Windows.

## 🚀 Quick Start

### 1. Environment Preparation

**Linux (Ubuntu/Debian):**

```bash
sudo apt update && sudo apt install libudev-dev pkg-config build-essential
```

**Linux (Fedora/CentOS):**

```bash
sudo dnf install systemd-devel
```

### 2. Scanning Device Information (Discovery)

Before writing the configuration, please run the tool to obtain the actual hardware information of the device.

```bash
# On Linux, it is recommended to use sudo to obtain full attributes
cargo run --example discovery
```

**Example Output:**

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

### 3. Configuration File (`device_config.json`)

```json
[
  {
    "role": "imu_sensor", // The name we configured, this field must be unique throughout the file
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

### 4. Code Integration

```rust
use usb_resolver::{get_monitor, DeviceRule, DeviceEvent};
use std::fs;

fn main() -> anyhow::Result<()> {
    // 1. Read configuration
    let config = fs::read_to_string("device_config.json")?;
    let rules: Vec<DeviceRule> = serde_json::from_str(&config)?;

    // 2. Get instance and channel
    let monitor = get_monitor();
    let (tx, rx) = crossbeam_channel::unbounded();

    // 3. Start background monitoring
    monitor.start(rules, tx)?;
    println!("Service started...");

    // 4. Event handling loop (Note: the main thread must not exit)
    for event in rx {
        match event {
            DeviceEvent::Attached(dev) => {
                println!("✅ Device connected: {}", dev.role);
                // Select the best opening path based on the platform
                let port = if cfg!(target_os = "windows") {
                    dev.device.system_path_alt.as_deref().unwrap_or(&dev.device.system_path)
                } else {
                    &dev.device.system_path
                };
                println!("   -> Port path: {}", port);
            },
            DeviceEvent::Detached(role) => {
                println!("❌ Device disconnected: {}", role);
            }
        }
    }
    Ok(())
}
```

## 🛠 Troubleshooting

1. **Device not detected on Linux?**
* **Permissions issue**: udev requires permissions. Try `sudo cargo run`.
* **Rules file**: If you don't want to run as root, configure `/etc/udev/rules.d/` to allow the current user access to USB devices.


2. **The program exits immediately after starting?**
* Once the Rust main thread ends, the background monitoring thread will also be killed. Make sure there is a `loop` or `rx.recv()` blocking operation in the main thread.


3. **`port_path` mismatch?** **
* The physical port path is related to the motherboard's USB topology. If you switch the device to a different USB port, the `port_path` will change. Please rerun the discovery tool to check.

4. **Cannot see debug logs in the TUI interface?**
* The TUI takes over standard output. It is recommended to configure the `log` library to write logs to a file (`WriteLogger`), and then use `tail -f debug.log` to view them.
