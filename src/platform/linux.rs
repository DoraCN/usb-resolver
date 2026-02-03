// Linux USB 监控原理：一切皆文件
// 在 Linux 上，我们和 “文件系统” 打交道。这体现了 Linux 的哲学：Everything is a file (一切皆文件)。
// Linux 的 USB 管理分两部分：
//     静态数据 (sysfs)：
//         位置：/sys/bus/usb/devices/...
//         这里存放着当前系统所有硬件的状态。
//         要查 VID，就读 idVendor 文件；要查 PID，就读 idProduct 文件。
//     动态事件 (udev / Netlink)：
//         当硬件插拔时，内核会通过一个特殊的 Socket（Netlink Socket）向用户空间广播消息。
//         udev 是 Linux 的设备管理器，它监听这个 Socket，处理这堆乱七八糟的消息，并对外提供更友好的接口。
// 前置要求： 在编译 Linux 版本之前，你的系统（或开发容器）里必须安装 libudev 的开发包。
//
// Debian/Ubuntu: sudo apt install libudev-dev
// Fedora: sudo dnf install systemd-devel
//

use std::{thread, time::Duration};

use udev::{Device, Enumerator};

use crate::RawDeviceInfo;

const ID_VENDOR: &str = "idVendor";
const ID_PRODUCT: &str = "idProduct";
const USB_SERIAL: &str = "serial";

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

    // Utility function for securely reading attributes
    // In Linux, attributes are OsStr, which we need to convert to String
    // 工具函数 安全读取属性
    // Linux 的属性是 OsStr，我们需要转换成 String
    fn get_property(dev: &Device, key: &str) -> Option<String> {
        dev.property_value(key)
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    }

    // Utility functions read sysfs attributes
    // Some attributes are not in the udev database, but in the /sys file
    // 工具函数 读取 sysfs 属性
    // 有些属性不在 udev 数据库里，而是在 /sys 文件里
    fn get_attribute(dev: &Device, key: &str) -> Option<String> {
        dev.attribute_value(key)
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    }

    // Recursively search for the TTY node
    // Given a USB device, find its corresponding /dev/ttyUSBx or /dev/ttyACMx
    // 递归查找 TTY 节点
    // 给定一个 USB 设备，找到它对应的 /dev/ttyUSBx 或者 /dev/ttyACMx
    fn find_tty_node(usb_dev: &Device) -> Option<String> {
        // Create an enumerator to iterate through the sub-devices.
        // 创建一个枚举器，用于遍历子设备
        let mut enumerator = Enumerator::new().ok()?;

        // Set matching criteria: It must be a child device of the current usb_dev.
        // 设置匹配条件：必须是当前 usb_dev 的子设备
        enumerator.match_parent(usb_dev).ok()?;

        // Set matching criteria: The subsystem type is "tty".
        // 设置匹配条件：子系统的类型是 "tty"
        enumerator.match_subsystem("tty").ok()?;

        // Traversal results
        // 遍历结果
        for child in enumerator.scan_devices().ok()? {
            // Return the first devnode found (e.g., /dev/ttyUSB0).
            // 找到第一个 devnode (例如 /dev/ttyUSB0) 就返回
            if let Some(devnode) = child.devnode() {
                return devnode.to_str().map(|s| s.to_string());
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

        // Read VID/PID (this is a hexadecimal string, such as "1a86")
        // 读取 VID / PID (这是 16 进制字符串，如 "1a86")
        let vid_str = Self::get_attribute(dev, ID_VENDOR)?;
        let pid_str = Self::get_attribute(dev, ID_PRODUCT)?;

        // Parsing hex strings
        // 解析 hex 字符串
        let vid = u16::from_str_radix(&vid_str, 16).ok()?;
        let pid = u16::from_str_radix(&pid_str, 16).ok()?;

        // Read Serial
        // 读取 Serial
        let serial = Self::get_attribute(dev, USB_SERIAL);

        // Get a unique ID
        // 获取唯一 ID
        let syspath = dev.syspath().to_str()?.to_string();

        // Get Port Path (physical port path)
        // In Linux, devpath is similar to "1-1.2", which is suitable for use as a port path.
        // 获取 Port Path (物理端口路径)
        // Linux 下 devpath 类似 "1-1.2"，直接用作 Port Path 很合适
        let port_path = dev
            .syspath()
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or("N/A".to_string());

        // Locate the TTY path (with retry mechanism). The driver may not be properly mounted when the device is first inserted.
        // 查找 TTY 路径 (带重试机制), 刚插入时可能驱动还没挂载好
        let mut tty_node = None;
        let max_retries = 20;

        for _ in 0..max_retries {
            if let Some(node) = Self::find_tty_node(dev) {
                tty_node = Some(node);
                break;
            }

            thread::sleep(Duration::from_millis(100));
        }

        Some(RawDeviceInfo {
            vid,
            pid,
            serial,
            port_path,
            system_path: syspath,      // primary key: /sys/devices/...
            system_path_alt: tty_node, // Actual path: /dev/ttyUSB0
        })
    }
}
