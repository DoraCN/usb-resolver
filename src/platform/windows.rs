use std::{collections::HashSet, thread, time::Duration};

use anyhow::Result;
use crossbeam_channel::Sender;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVINFO_DATA, SPDRP_FRIENDLYNAME,
    SPDRP_HARDWAREID, SPDRP_LOCATION_INFORMATION, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
    SetupDiGetDeviceRegistryPropertyW,
};

use crate::{DeviceEvent, DeviceMonitor, RawDeviceInfo};

pub struct WindowsMonitor;

impl Default for WindowsMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsMonitor {
    pub fn new() -> Self {
        Self
    }

    // 从Windows属性中读取字符串
    pub fn get_device_property(
        dev_info: HDEVINFO,
        dev_data: &mut SP_DEVINFO_DATA,
        property: u32,
    ) -> Option<String> {
        // 准备缓冲区
        let mut buffer = [0u8; 1024];
        let mut required_size = 0u32;

        // 调用Windows API获取属性
        let success = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                dev_info,
                dev_data,
                property,
                None,
                Some(&mut buffer),
                Some(&mut required_size),
            )
        };

        if success.is_err() {
            return None;
        }

        // 将缓冲区转换为字符串
        // Windows 返回的是 UTF-16 (u16)，但它以 u8 形式存在 buffer 里。
        let len = (required_size as usize) / 2; // WCHAR 是2字节
        if len == 0 {
            return None;
        }

        // 安全地将 u8 切片转换为 u16 切片
        let ptr = buffer.as_ptr() as *const u16;
        let u16_slice = unsafe { std::slice::from_raw_parts(ptr, len) };

        // 转换为 Rust 字符串并去除尾部的空字符
        // 注意：Windows API 返回的字符串通常以 null 结尾
        // 我们使用 trim_matches 去除这些 null 字符
        String::from_utf16(u16_slice)
            .ok()
            .map(|s| s.trim_matches(char::from(0)).to_string())
    }

    //获取设备实例 ID (唯一路径)
    // 结果示例: "USB\VID_1234&PID_5678\SN001" (有序列号)
    // 或者:    "USB\VID_1234&PID_5678\5&2A3B4C5D&0&1" (无序列号，系统生成)
    fn get_instance_id(dev_info: HDEVINFO, dev_data: &mut SP_DEVINFO_DATA) -> Option<String> {
        let mut buffer = [0u16; 1024]; // 直接用 u16 数组
        let mut required_size = 0;

        let result = unsafe {
            SetupDiGetDeviceInstanceIdW(
                dev_info,
                dev_data,
                Some(&mut buffer),
                Some(&mut required_size),
            )
        };

        if result.is_err() {
            return None;
        }

        let len = required_size as usize;
        if len == 0 {
            return None;
        }

        // 转为 String
        String::from_utf16(&buffer[..len])
            .ok()
            .map(|s| s.trim_matches(char::from(0)).to_string())
    }

    // 解析 HardwareID 字符串
    fn parse_hardware_id(id_str: &str) -> Option<(u16, u16)> {
        // 示例 ID 字符串格式: "USB\VID_1AE4&PID_56B2&REV_0100"
        // 1. 必须包含 "VID_" 和 "PID_"
        let vid_index = id_str.find("VID_")?;
        let pid_index = id_str.find("PID_")?;

        // 2. 提取 VID (4位 hex)
        // "VID_" 后面紧跟的 4 个字符
        let vid_start = vid_index + 4;
        if vid_start + 4 > id_str.len() {
            return None;
        }
        let vid_str = &id_str[vid_start..vid_start + 4];
        let vid = u16::from_str_radix(vid_str, 16).ok()?;

        // 3. 提取 PID (4位 hex)
        // "PID_" 后面紧跟的 4 个字符
        let pid_start = pid_index + 4;
        if pid_start + 4 > id_str.len() {
            return None;
        }
        let pid_str = &id_str[pid_start..pid_start + 4];
        let pid = u16::from_str_radix(pid_str, 16).ok()?;

        Some((vid, pid))
    }

    fn scan_now() -> Result<Vec<RawDeviceInfo>> {
        let mut devices = vec![];

        let device_info_set =
            unsafe { SetupDiGetClassDevsW(None, None, None, DIGCF_PRESENT | DIGCF_ALLCLASSES) }?;

        let mut dev_data = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };

        let mut index = 0;

        while unsafe { SetupDiEnumDeviceInfo(device_info_set, index, &mut dev_data).is_ok() } {
            index += 1;

            let hardware_id_raw = Self::get_device_property(
                device_info_set,
                &mut dev_data,
                SPDRP_HARDWAREID, // 1
            );

            let hardware_id = match hardware_id_raw {
                Some(id) => id,
                None => continue,
            };
            // 这里的 ToUppercase 是为了防御性编程，防止某些厂商大小写乱写
            if !hardware_id.to_uppercase().starts_with("USB") {
                continue;
            }

            // 解析 VID / PID
            let (vid, pid) = match Self::parse_hardware_id(&hardware_id) {
                Some(res) => res,
                None => continue, // 解析失败说明格式不对，跳过
            };

            // 读取 Instance ID (作为系统主路径 / 唯一 ID)
            // 示例: "USB\VID_1234&PID_5678\SN_001"
            // Windows 下这是绝对唯一的，适合做 System Path
            // 让我们读 "FriendlyName" (友好名称)，里面通常包含 COM 口号
            // 示例: "USB Serial Device (COM3)"
            let friendly_name = Self::get_device_property(
                device_info_set,
                &mut dev_data,
                SPDRP_FRIENDLYNAME, // 12
            );

            // 读取 Location Information (作为物理端口路径)
            // 示例: "Port_#0002.Hub_#0001"
            let port_path = Self::get_device_property(
                device_info_set,
                &mut dev_data,
                SPDRP_LOCATION_INFORMATION, // 13
            )
            .unwrap_or_else(|| "unknown".to_string());

            // 尝试从 FriendlyName 提取 COM 口 (作为备用路径)
            // 如果 friendly_name 里包含 "(COM", 我们就把它提取出来
            let system_path_alt = friendly_name.as_ref().and_then(|name| {
                let start = name.find("(COM")?;
                let end = name[start..].find(")")?;
                // 提取 "COM3"
                Some(name[start + 1..start + end].to_string())
            });

            // 构造 system_path (这里用 hardware_id 做前缀，如果能拿到 InstanceId 更好)
            // 为了唯一性，如果能读到 Serial Number 最好
            // 这里简化处理：用 HardwareID 作为 system_path 的一部分
            let system_path = hardware_id.clone();

            // 读取 Instance ID (作为真正的 System Path)
            // 之前的 hardware_id 只是"型号"，Instance ID 才是"绝对路径"
            // 示例: "USB\VID_1234&PID_5678\12345678"
            let instance_id = match Self::get_instance_id(device_info_set, &mut dev_data) {
                Some(id) => id,
                None => continue,
            };

            // 尝试读取序列号 (Windows 没有直接的 Serial 属性，通常藏在 Instance ID 的最后一段)
            let serial = instance_id
                .rsplit('\\') // 从右向左找第一个 '\'
                .next()
                .map(|s| s.to_string())
                .filter(|s| !s.contains('&')); // 过滤掉 Windows 生成的 "5&2d..." 这种伪序列号

            devices.push(RawDeviceInfo {
                vid,
                pid,
                serial,
                port_path,
                system_path,
                system_path_alt,
            });
        }

        // 释放内存 (非常重要，否则内存泄漏)
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(device_info_set);
        };

        Ok(devices)
    }
}

impl DeviceMonitor for WindowsMonitor {
    fn start(&self, tx: Sender<DeviceEvent>) -> Result<()> {
        // 1. 初始扫描 (补发存量)
        let mut known_devices: HashSet<String> = HashSet::new();

        if let Ok(devices) = self.scan_now() {
            for dev in devices {
                known_devices.insert(dev.system_path.clone());
                tx.send(DeviceEvent::Attached(dev)).ok();
            }
        }

        // 2. 启动轮询线程
        thread::spawn(move || {
            loop {
                // 1秒轮询一次
                thread::sleep(Duration::from_secs(1));

                // 再次扫描
                let current_devices = match Self::scan_now() {
                    Ok(devs) => devs,
                    Err(e) => {
                        eprintln!("[Windows] Scan failed: {:?}", e);
                        continue;
                    }
                };

                // 找出现在的 ID 集合
                let current_ids: HashSet<String> = current_devices
                    .iter()
                    .map(|d| d.system_path.clone())
                    .collect();

                // Check A: 新增的设备 (Present now but not before)
                for dev in &current_devices {
                    if !known_devices.contains(&dev.system_path) {
                        tx.send(DeviceEvent::Attached(dev.clone())).ok();
                        known_devices.insert(dev.system_path.clone());
                    }
                }

                // Check B: 移除的设备 (Present before but not now)
                // retain 的逻辑是：保留返回 true 的，删除返回 false 的
                // 我们利用这个副作用来发送移除事件
                known_devices.retain(|old_path| {
                    if !current_ids.contains(old_path) {
                        tx.send(DeviceEvent::Detached(old_path.clone())).ok();
                        return false; // 从 known_devices 删除
                    }
                    true // 保留
                });
            }
        });

        Ok(())
    }

    fn scan_now(&self) -> Result<Vec<RawDeviceInfo>> {
        Self::scan_now()
    }
}
