#[cfg(target_os = "windows")]
use crate::{DeviceEvent, DeviceMonitor, DeviceRule, RawDeviceInfo, ResolvedDevice};
#[cfg(target_os = "windows")]
use anyhow::Result;
#[cfg(target_os = "windows")]
use crossbeam_channel::Sender;
#[cfg(target_os = "windows")]
use std::collections::HashMap;
#[cfg(target_os = "windows")]
use std::ffi::OsString;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStringExt;
#[cfg(target_os = "windows")]
use std::thread;
#[cfg(target_os = "windows")]
use std::time::Duration;

#[cfg(target_os = "windows")]
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CR_SUCCESS, DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVINFO_DATA,
    SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID, SPDRP_LOCATION_PATHS, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
};

#[cfg(target_os = "windows")]
pub struct WindowsMonitor;

impl Default for WindowsMonitor {
    fn default() -> Self {
        Self
    }
}

#[cfg(target_os = "windows")]
impl WindowsMonitor {
    pub fn new() -> Self {
        Self
    }

    fn trim_wide_string(buffer: &[u16], len: usize) -> String {
        let actual_len = if len > 0 && buffer[len - 1] == 0 {
            len - 1
        } else {
            len
        };
        OsString::from_wide(&buffer[..actual_len])
            .to_string_lossy()
            .into_owned()
    }

    fn parse_dev_info(hdevinfo: HDEVINFO, devinfo_data: &SP_DEVINFO_DATA) -> Option<RawDeviceInfo> {
        unsafe {
            let hw_id =
                Self::get_device_registry_property(hdevinfo, devinfo_data, SPDRP_HARDWAREID)?;

            // 过滤：只保留包含 VID_ 的设备
            if !hw_id.to_uppercase().contains("VID_") {
                return None;
            }

            let (vid, pid) = Self::extract_vid_pid(&hw_id)?;

            let mut instance_id_buffer = [0u16; 256];
            if CM_Get_Device_IDW(devinfo_data.DevInst, &mut instance_id_buffer, 0) != CR_SUCCESS {
                return None;
            }
            let end = instance_id_buffer
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(256);
            let instance_id = OsString::from_wide(&instance_id_buffer[..end])
                .to_string_lossy()
                .into_owned();

            let serial = instance_id.split('\\').next_back().map(|s| s.to_string());

            let port_path =
                Self::get_device_registry_property(hdevinfo, devinfo_data, SPDRP_LOCATION_PATHS)
                    .unwrap_or_else(|| "unknown".to_string());

            let friendly_name =
                Self::get_device_registry_property(hdevinfo, devinfo_data, SPDRP_FRIENDLYNAME);

            let com_port = friendly_name.as_ref().and_then(|name| {
                if let Some(start) = name.find("(COM")
                    && let Some(end) = name[start..].find(")")
                {
                    return Some(name[start + 1..start + end].to_string());
                }
                None
            });

            Some(RawDeviceInfo {
                vid,
                pid,
                serial,
                port_path,
                system_path: instance_id,
                system_path_alt: com_port,
            })
        }
    }

    unsafe fn get_device_registry_property(
        hdevinfo: HDEVINFO,
        devinfo_data: &SP_DEVINFO_DATA,
        prop: u32,
    ) -> Option<String> {
        let mut required_len = 0;
        let _ = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                hdevinfo,
                devinfo_data,
                prop,
                None,
                None,
                Some(&mut required_len),
            )
        };

        if required_len == 0 {
            return None;
        }

        let mut buffer = vec![0u8; required_len as usize];
        let result = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                hdevinfo,
                devinfo_data,
                prop,
                None,
                Some(buffer.as_mut_slice()),
                None,
            )
        };

        if result.is_ok() {
            let wide_buffer: Vec<u16> = buffer
                .chunks_exact(2)
                .map(|chunk| u16::from_ne_bytes([chunk[0], chunk[1]]))
                .collect();
            Some(Self::trim_wide_string(&wide_buffer, wide_buffer.len()))
        } else {
            None
        }
    }

    fn extract_vid_pid(hw_id: &str) -> Option<(u16, u16)> {
        let hw_id_upper = hw_id.to_uppercase();
        let vid_idx = hw_id_upper.find("VID_")?;
        let pid_idx = hw_id_upper.find("PID_")?;

        if vid_idx + 8 > hw_id_upper.len() || pid_idx + 8 > hw_id_upper.len() {
            return None;
        }

        let vid_str = &hw_id_upper[vid_idx + 4..vid_idx + 8];
        let pid_str = &hw_id_upper[pid_idx + 4..pid_idx + 8];

        let vid = u16::from_str_radix(vid_str, 16).ok()?;
        let pid = u16::from_str_radix(pid_str, 16).ok()?;

        Some((vid, pid))
    }
}

#[cfg(target_os = "windows")]
impl DeviceMonitor for WindowsMonitor {
    fn scan_now(&self) -> Result<Vec<RawDeviceInfo>> {
        unsafe {
            // 广谱扫描：不传 Enumerator，只用 ALLCLASSES | PRESENT
            let hdevinfo =
                SetupDiGetClassDevsW(None, None, None, DIGCF_ALLCLASSES | DIGCF_PRESENT)?;

            let mut devices = Vec::new();
            let mut devinfo_data = SP_DEVINFO_DATA {
                cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };

            let mut i = 0;
            while SetupDiEnumDeviceInfo(hdevinfo, i, &mut devinfo_data).is_ok() {
                if let Some(info) = Self::parse_dev_info(hdevinfo, &devinfo_data) {
                    devices.push(info);
                }
                i += 1;
            }

            let _ = SetupDiDestroyDeviceInfoList(hdevinfo);
            Ok(devices)
        }
    }

    fn start(&self, rules: Vec<DeviceRule>, tx: Sender<DeviceEvent>) -> Result<()> {
        let mut active_roles: HashMap<String, String> = HashMap::new();

        // 1. 初始扫描
        log::info!("正在执行初始设备扫描...");
        if let Ok(initial_devices) = self.scan_now() {
            for dev in initial_devices {
                for rule in &rules {
                    if let Some(method) = rule.matches(&dev)
                        && !active_roles.contains_key(&rule.role)
                    {
                        active_roles.insert(rule.role.clone(), dev.system_path.clone());
                        tx.send(DeviceEvent::Attached(ResolvedDevice {
                            role: rule.role.clone(),
                            device: dev,
                            match_method: method,
                        }))
                        .ok();
                        break;
                    }
                }
            }
        }

        // 2. 启动轮询线程 (Polling Thread)
        // 相比于复杂的 Windows 消息回调，轮询在 CLI 工具中更稳定、更不容易崩溃
        let monitor = WindowsMonitor::new();

        thread::spawn(move || {
            log::info!("[Windows] 启动轮询监控模式，扫描间隔: 1秒");
            loop {
                thread::sleep(Duration::from_secs(1));

                // 执行全量扫描
                let current_devices = match monitor.scan_now() {
                    Ok(devs) => devs,
                    Err(e) => {
                        log::error!("扫描失败: {:?}", e);
                        continue;
                    }
                };

                // --- Diff 算法 ---

                // 1. 检测【新上线】
                for dev in &current_devices {
                    for rule in &rules {
                        if let Some(method) = rule.matches(dev) {
                            // 如果 active_roles 里没有这个 role，说明是新设备
                            if !active_roles.contains_key(&rule.role) {
                                log::info!("[Windows] 设备上线: {}", rule.role);
                                active_roles.insert(rule.role.clone(), dev.system_path.clone());
                                tx.send(DeviceEvent::Attached(ResolvedDevice {
                                    role: rule.role.clone(),
                                    device: dev.clone(),
                                    match_method: method,
                                }))
                                .ok();
                            }
                            break; // 匹配到一个规则就跳出规则循环
                        }
                    }
                }

                // 2. 检测【已下线】
                // 遍历 active_roles，如果它们对应的 system_path (InstanceID) 不在 current_devices 里，说明拔掉了
                let mut removed_roles = Vec::new();
                for (role, path) in active_roles.iter() {
                    if !current_devices.iter().any(|d| &d.system_path == path) {
                        removed_roles.push(role.clone());
                    }
                }

                for role in removed_roles {
                    log::info!("[Windows] 设备下线: {}", role);
                    active_roles.remove(&role);
                    tx.send(DeviceEvent::Detached(role)).ok();
                }
            }
        });

        Ok(())
    }
}
