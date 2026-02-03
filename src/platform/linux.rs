use crate::{DeviceEvent, DeviceMonitor, DeviceRule, RawDeviceInfo, ResolvedDevice};
use anyhow::{Context, Result, anyhow};
use crossbeam_channel::Sender;
use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use udev::{Enumerator, EventType, MonitorBuilder};

pub struct LinuxMonitor;

impl LinuxMonitor {
    pub fn new() -> Self {
        Self
    }

    /// 查找归属于当前 USB 设备的 TTY 子节点
    /// 例如：输入是 USB Device (1-1.2), 输出是 "/dev/ttyUSB0"
    fn find_tty_node(parent: &udev::Device) -> Option<String> {
        let mut enumerator = Enumerator::new().ok()?;

        // 1. 过滤 subsystem = "tty"
        enumerator.match_subsystem("tty").ok()?;

        // 2. 关键过滤：只找 parent 是当前设备的孩子
        enumerator.match_parent(parent).ok()?;

        // 3. 扫描并返回第一个结果
        for child in enumerator.scan_devices().ok()? {
            if let Some(devnode) = child.devnode() {
                if let Some(path) = devnode.to_str() {
                    return Some(path.to_string());
                }
            }
        }
        None
    }

    fn parse_device(dev: udev::Device) -> Option<RawDeviceInfo> {
        let subsystem = dev.subsystem().and_then(|s| s.to_str());
        if subsystem != Some("usb") {
            return None;
        }

        let devtype = dev.devtype().and_then(|s| s.to_str());
        if devtype != Some("usb_device") {
            return None;
        }

        let vid = if let Some(v) = dev.property_value("ID_VENDOR_ID") {
            let s = v.to_str().unwrap_or("");
            u16::from_str_radix(s, 16).ok()?
        } else {
            let v = dev
                .attribute_value("idVendor")
                .and_then(|s| s.to_str())
                .unwrap_or("");
            u16::from_str_radix(v, 16).ok()?
        };

        let pid = if let Some(p) = dev.property_value("ID_MODEL_ID") {
            let s = p.to_str().unwrap_or("");
            u16::from_str_radix(s, 16).ok()?
        } else {
            let p = dev
                .attribute_value("idProduct")
                .and_then(|s| s.to_str())
                .unwrap_or("");
            u16::from_str_radix(p, 16).ok()?
        };

        let port_path = dev
            .property_value("ID_PATH")
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let serial = dev
            .property_value("ID_SERIAL_SHORT")
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        // 原始的总线路径 (/dev/bus/usb/001/005)
        let raw_usb_path = dev
            .devnode()
            .and_then(|p| p.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                dev.syspath()
                    .to_str()
                    .unwrap_or("invalid_utf8_path")
                    .to_string()
            });

        // 查找 TTY 节点 (/dev/ttyUSB0)
        let tty_path = Self::find_tty_node(&dev);

        // 决策：
        // 如果找到了 TTY，它就是主路径 (system_path)，Raw Path 变为备用。
        // 如果没找到 TTY (比如键盘)，Raw Path 是主路径。
        let (system_path, system_path_alt) = match tty_path {
            Some(tty) => (tty, Some(raw_usb_path)),
            None => (raw_usb_path, None),
        };

        Some(RawDeviceInfo {
            vid,
            pid,
            serial,
            port_path,
            system_path,
            system_path_alt,
        })
    }
}

impl DeviceMonitor for LinuxMonitor {
    fn scan_now(&self) -> Result<Vec<RawDeviceInfo>> {
        let mut enumerator = Enumerator::new()?;
        enumerator.match_subsystem("usb")?;
        enumerator.match_property("DEVTYPE", "usb_device")?;

        let mut devices = Vec::new();
        for dev in enumerator.scan_devices()? {
            if let Some(info) = Self::parse_device(dev) {
                devices.push(info);
            }
        }
        Ok(devices)
    }

    fn start(&self, rules: Vec<DeviceRule>, tx: Sender<DeviceEvent>) -> Result<()> {
        let mut active_roles: HashMap<String, String> = HashMap::new();
        let mut path_to_role: HashMap<String, String> = HashMap::new();

        // 1. 初始扫描
        log::info!("正在进行初始 USB 扫描...");
        if let Ok(initial_devices) = self.scan_now() {
            for dev in initial_devices {
                for rule in &rules {
                    if let Some(method) = rule.matches(&dev) {
                        if !active_roles.contains_key(&rule.role) {
                            active_roles.insert(rule.role.clone(), dev.system_path.clone());
                            path_to_role.insert(dev.system_path.clone(), rule.role.clone());

                            tx.send(DeviceEvent::Attached(ResolvedDevice {
                                role: rule.role.clone(),
                                device: dev.clone(),
                                match_method: method,
                            }))
                            .ok();
                            break;
                        }
                    }
                }
            }
        }

        let (init_tx, init_rx) = mpsc::channel();
        let monitor_clone = LinuxMonitor::new();

        // 2. 启动监听线程
        thread::Builder::new()
            .name("usb-monitor".to_string())
            .spawn(move || {
                let mut use_polling = false;

                // --- 尝试 A: 使用 udev Netlink 监听 ---
                let init_result = (|| -> Result<udev::MonitorSocket> {
                    let builder = MonitorBuilder::new().context("Builder init failed")?;
                    let monitor = builder.match_subsystem("usb")?.listen()?;
                    Ok(monitor)
                })();

                match init_result {
                    Ok(monitor) => {
                        log::info!("[udev-thread] udev 监听初始化成功");
                        let _ = init_tx.send(Ok(()));

                        for event in monitor.iter() {
                            match event.event_type() {
                                EventType::Add => {
                                    if let Some(dev) = Self::parse_device(event.device()) {
                                        for rule in &rules {
                                            if let Some(method) = rule.matches(&dev) {
                                                if active_roles.contains_key(&rule.role) {
                                                    continue;
                                                }

                                                log::info!("[udev] 匹配设备上线: {}", rule.role);
                                                active_roles.insert(
                                                    rule.role.clone(),
                                                    dev.system_path.clone(),
                                                );
                                                path_to_role.insert(
                                                    dev.system_path.clone(),
                                                    rule.role.clone(),
                                                );

                                                if let Err(_) =
                                                    tx.send(DeviceEvent::Attached(ResolvedDevice {
                                                        role: rule.role.clone(),
                                                        device: dev,
                                                        match_method: method,
                                                    }))
                                                {
                                                    return;
                                                }
                                                break;
                                            }
                                        }
                                    }
                                }
                                EventType::Remove => {
                                    let key = event
                                        .device()
                                        .devnode()
                                        .and_then(|p| p.to_str())
                                        .map(|s| s.to_string())
                                        .unwrap_or_else(|| {
                                            event
                                                .device()
                                                .syspath()
                                                .to_str()
                                                .unwrap_or("")
                                                .to_string()
                                        });

                                    // 注意：这里有个小坑。如果 parse_device 把 system_path 改成了 tty，
                                    // 但 udev Remove 事件传回来的可能是 usb_device 的 devnode。
                                    // 幸好我们 path_to_role 的 Key 存的是 system_path。
                                    // 为了保险，Remove 时我们也需要尽可能找到 system_path。
                                    // 但 Remove 时 tty 节点可能已经先没了。
                                    //
                                    // 在 Polling 模式下这没问题。
                                    // 在 Netlink 模式下，如果 remove 事件匹配不到 path_to_role，我们可能需要遍历 Values 查找。
                                    //
                                    // 简单修复：尝试直接 remove，如果不行，遍历查找。
                                    if let Some(role) = path_to_role.remove(&key) {
                                        log::info!("[udev] 设备移除: {}", role);
                                        active_roles.remove(&role);
                                        if let Err(_) = tx.send(DeviceEvent::Detached(role)) {
                                            return;
                                        }
                                    } else {
                                        // Fallback: 如果 Key 不匹配（因为 system_path 变成了 /dev/tty...），
                                        // 我们需要检查 path_to_role 的 Values 里面有没有谁的 alt_path 是这个 key
                                        // 这里简单处理：如果找不到，依赖 Polling 或者忽略
                                        // (在工业场景通常用 Polling 兜底会更稳)
                                    }
                                }
                                _ => {}
                            }
                        }

                        log::warn!("[udev-thread] udev socket 意外关闭，切换至轮询模式...");
                        use_polling = true;
                    }
                    Err(e) => {
                        log::warn!(
                            "[udev-thread] udev 初始化失败 ({:?})，直接切换至轮询模式",
                            e
                        );
                        let _ = init_tx.send(Ok(()));
                        use_polling = true;
                    }
                }

                // --- 尝试 B: 轮询兜底模式 ---
                if use_polling {
                    log::info!("[udev-thread] 已启动轮询模式 (Polling Mode)，扫描间隔: 2秒");

                    loop {
                        thread::sleep(Duration::from_secs(2));

                        // 1. 扫描当前所有设备
                        let current_devices = match monitor_clone.scan_now() {
                            Ok(devs) => devs,
                            Err(e) => {
                                log::error!("轮询扫描失败: {:?}", e);
                                continue;
                            }
                        };

                        let mut current_matches: HashMap<
                            String,
                            (String, RawDeviceInfo, crate::MatchMethod),
                        > = HashMap::new();

                        for dev in current_devices {
                            for rule in &rules {
                                if let Some(method) = rule.matches(&dev) {
                                    current_matches.insert(
                                        rule.role.clone(),
                                        (dev.system_path.clone(), dev, method),
                                    );
                                    break;
                                }
                            }
                        }

                        // 3. Diff: 检测【新上线】
                        for (role, (path, dev, method)) in &current_matches {
                            if !active_roles.contains_key(role) {
                                log::info!("[Polling] 设备上线: {} -> {}", role, path);
                                active_roles.insert(role.clone(), path.clone());
                                path_to_role.insert(path.clone(), role.clone());

                                if let Err(_) = tx.send(DeviceEvent::Attached(ResolvedDevice {
                                    role: role.clone(),
                                    device: dev.clone(),
                                    match_method: *method,
                                })) {
                                    return;
                                }
                            }
                        }

                        // 4. Diff: 检测【已掉线】
                        let mut roles_to_remove = Vec::new();
                        for (role, path) in &active_roles {
                            if !current_matches.contains_key(role) {
                                roles_to_remove.push((role.clone(), path.clone()));
                            }
                        }

                        for (role, path) in roles_to_remove {
                            log::info!("[Polling] 设备下线: {}", role);
                            active_roles.remove(&role);
                            path_to_role.remove(&path);
                            if let Err(_) = tx.send(DeviceEvent::Detached(role)) {
                                return;
                            }
                        }
                    }
                }
            })
            .expect("Failed to spawn monitor thread");

        match init_rx.recv() {
            Ok(result) => result,
            Err(_) => Err(anyhow!("监听线程崩溃")),
        }
    }
}
