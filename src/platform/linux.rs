// Linux USB 监控原理: 一切皆文件
// 在 Linux 上, 我们和 “文件系统” 打交道。这体现了 Linux 的哲学: Everything is a file (一切皆文件)。
// Linux 的 USB 管理分两部分:
//     静态数据 (sysfs):
//         位置: /sys/bus/usb/devices/...
//         这里存放着当前系统所有硬件的状态。
//         要查 VID, 就读 idVendor 文件；要查 PID, 就读 idProduct 文件。
//     动态事件 (udev / Netlink):
//         当硬件插拔时, 内核会通过一个特殊的 Socket(Netlink Socket)向用户空间广播消息。
//         udev 是 Linux 的设备管理器, 它监听这个 Socket, 处理这堆乱七八糟的消息, 并对外提供更友好的接口。
// 前置要求: 在编译 Linux 版本之前, 你的系统(或开发容器)里必须安装 libudev 的开发包。
//
// Debian/Ubuntu: sudo apt install libudev-dev
// Fedora: sudo dnf install systemd-devel
//

use std::{thread, time::Duration};

use anyhow::Result;
use crossbeam_channel::Sender;
use log::info;
use udev::{Device, Enumerator, EventType};

use crate::{DeviceEvent, DeviceMonitor, RawDeviceInfo};

const ID_VENDOR: &str = "idVendor";
const ID_PRODUCT: &str = "idProduct";
const USB_SERIAL: &str = "serial";

const ID_VENDOR_ID: &str = "ID_VENDOR_ID";
const ID_MODEL_ID: &str = "ID_MODEL_ID";
const ID_SERIAL_SHORT: &str = "ID_SERIAL_SHORT";
const ID_PATH: &str = "ID_PATH";

pub struct LinuxMonitor;

impl Default for LinuxMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxMonitor {
    pub fn new() -> Self {
        Self
    }

    // Recursively search for the TTY node
    // Given a USB device, find its corresponding /dev/ttyUSBx or /dev/ttyACMx
    // 递归查找 TTY 节点
    // 给定一个 USB 设备, 找到它对应的 /dev/ttyUSBx 或者 /dev/ttyACMx
    fn find_tty_node(usb_dev: &Device) -> Option<String> {
        // Create an enumerator to iterate through the sub-devices.
        // 创建一个枚举器, 用于遍历子设备
        let mut enumerator = Enumerator::new().ok()?;

        // Set matching criteria: The subsystem type is "tty".
        // 设置匹配条件: 子系统的类型是 "tty"
        enumerator.match_subsystem("tty").ok()?;

        // Set matching criteria: It must be a child device of the current usb_dev.
        // 设置匹配条件: 必须是当前 usb_dev 的子设备
        enumerator.match_parent(usb_dev).ok()?;

        // Traversal results
        // 遍历结果
        for child in enumerator.scan_devices().ok()? {
            if let Some(devnode) = child.devnode()
                && let Some(path) = devnode.to_str()
            {
                return Some(path.to_string());
            }
        }

        None
    }

    // parse device
    // 解析设备
    fn parse_device(dev: &Device) -> Option<RawDeviceInfo> {
        // Only devices with the "USB" subsystem have VID/PID.
        // 只有 "usb" 子系统的设备才有 VID/PID
        if dev.subsystem().and_then(|s| s.to_str()) != Some("usb") {
            return None;
        }

        // Only devices of type "usb_device" are considered physical devices (excluding usb_interface).
        // 只有 "usb_device" 类型才算物理设备 (排除 usb_interface)
        if dev.devtype().and_then(|s| s.to_str()) != Some("usb_device") {
            return None;
        }

        // Read VID/PID
        // 读取 VID / PID
        let vid = if let Some(v) = dev.property_value(ID_VENDOR_ID) {
            let s = v.to_str().unwrap_or("");
            u16::from_str_radix(s, 16).ok()?
        } else {
            let v = dev
                .attribute_value(ID_VENDOR)
                .and_then(|s| s.to_str())
                .unwrap_or("");
            u16::from_str_radix(v, 16).ok()?
        };
        let pid = if let Some(p) = dev.property_value(ID_MODEL_ID) {
            let s = p.to_str().unwrap_or("");
            u16::from_str_radix(s, 16).ok()?
        } else {
            let p = dev
                .attribute_value(ID_PRODUCT)
                .and_then(|s| s.to_str())
                .unwrap_or("");
            u16::from_str_radix(p, 16).ok()?
        };

        // Read Serial
        // 读取 Serial
        let serial = dev
            .property_value(ID_SERIAL_SHORT)
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .or_else(|| {
                dev.attribute_value(USB_SERIAL)
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            });

        // 物理端口路径
        let port_path = dev
            .property_value(ID_PATH)
            .and_then(|s| s.to_str())
            .unwrap_or("N/A")
            .to_string();

        // The original bus path (/dev/bus/usb/001/005)
        // 原始的总线路径 (/dev/bus/usb/001/005)
        let syspath = dev.syspath().to_str()?.to_string();

        // Locate the TTY path
        // 查找 TTY 路径
        let tty_path = Self::find_tty_node(dev);

        Some(RawDeviceInfo {
            vid,
            pid,
            serial,
            port_path,
            system_path: syspath,      // primary key: /sys/devices/...
            system_path_alt: tty_path, // Actual path: /dev/ttyUSB0
        })
    }
}

impl DeviceMonitor for LinuxMonitor {
    fn start(&self, tx: Sender<DeviceEvent>) -> Result<()> {
        if let Ok(devices) = self.scan_now() {
            for device in devices {
                tx.send(DeviceEvent::Attached(device)).ok();
            }
        }

        thread::spawn(move || {
            // Create a Builder and configure filtering rules.
            // 创建 Builder, 配置过滤规则
            let builder = match udev::MonitorBuilder::new()
                // .and_then(|b| b.match_subsystem_devtype("usb", "usb_device"))
                .and_then(|b| b.match_subsystem("usb"))
            {
                Ok(b) => b,
                Err(e) => {
                    info!("[Error] Failed to create udev builder: {:?}", e);
                    return;
                }
            };

            let monitor = match builder.listen() {
                Ok(m) => m,
                Err(e) => {
                    info!("[Error] Failed to listen to udev socket: {:?}", e);
                    return;
                }
            };

            info!("[Linux] The udev listener thread has been started.");

            loop {
                for event in monitor.iter() {
                    let event_type = event.event_type();
                    let device = event.device();

                    // Debug: 看看到底收到了什么事件
                    info!("[Event] {:?} -> {:?}", event_type, device.syspath());

                    match event.event_type() {
                        // Insertion event
                        // 插入事件
                        EventType::Add => {
                            if let Some(dev) = Self::parse_device(&event.device()) {
                                tx.send(DeviceEvent::Attached(dev)).ok();
                            }
                        }
                        // Remove event
                        // 移除事件
                        EventType::Remove => {
                            if let Some(path_str) = event.device().syspath().to_str() {
                                tx.send(DeviceEvent::Detached(path_str.to_string())).ok();
                            }
                        }
                        _ => {}
                    }
                }

                std::thread::sleep(Duration::from_millis(200));
            }
        });

        Ok(())
    }

    fn scan_now(&self) -> Result<Vec<RawDeviceInfo>> {
        // Create an enumeration
        // 创建枚举
        let mut enumerator = Enumerator::new()?;

        // Only devices with the "USB" subsystem are relevant.
        // 只有 "usb" 子系统的设备才相关
        enumerator.match_subsystem("usb")?;

        // Key filtering: Only view "usb_device" (physical device)
        // Ignore child nodes such as usb_interface, usb_endpoint
        // 键过滤: 只看 "usb_device" (物理设备)
        // 忽略 usb_interface, usb_endpoint 等子节点
        enumerator.match_property("DEVTYPE", "usb_device")?;

        // Scan and collect
        // 扫描并收集
        let mut devices = vec![];
        for device in enumerator.scan_devices()? {
            if let Some(info) = Self::parse_device(&device) {
                devices.push(info);
            }
        }

        Ok(devices)
    }
}
