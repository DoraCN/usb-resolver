// src/platform/macos.rs

#[cfg(target_os = "macos")]
use crate::{DeviceMonitor, DeviceRule, DeviceEvent, RawDeviceInfo, ResolvedDevice};
#[cfg(target_os = "macos")]
use anyhow::{anyhow, Result};
#[cfg(target_os = "macos")]
use crossbeam_channel::Sender;
#[cfg(target_os = "macos")]
use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::ffi::{c_void, CStr, CString};
#[cfg(target_os = "macos")]
use std::sync::Mutex;
#[cfg(target_os = "macos")]
use std::thread;
#[cfg(target_os = "macos")]
use std::time::Duration; // 引入 Duration

// ... 其他引用保持不变 ...
#[cfg(target_os = "macos")]
use core_foundation::base::{kCFAllocatorDefault, CFRelease, CFTypeRef};
#[cfg(target_os = "macos")]
use core_foundation::number::{kCFNumberSInt32Type, CFNumberGetValue, CFNumberRef};
#[cfg(target_os = "macos")]
use core_foundation::string::{
    kCFStringEncodingUTF8, CFStringGetCString, CFStringGetLength,
    CFStringRef, CFStringCreateWithCString,
};
#[cfg(target_os = "macos")]
use core_foundation::runloop::{CFRunLoopRun, CFRunLoopGetCurrent, kCFRunLoopDefaultMode, CFRunLoopAddSource};
#[cfg(target_os = "macos")]
use io_kit_sys::{
    kIOMasterPortDefault,
    IONotificationPortCreate, IONotificationPortGetRunLoopSource,
    IOObjectRelease, IOIteratorNext,
    IORegistryEntryCreateCFProperty, IORegistryEntryGetPath,
    IOServiceAddMatchingNotification, IOServiceGetMatchingServices, IOServiceMatching,
    IORegistryEntryCreateIterator,
};
#[cfg(target_os = "macos")]
use mach::port::mach_port_t;

#[cfg(target_os = "macos")]
#[allow(non_camel_case_types)]
type io_iterator_t = mach_port_t;
#[cfg(target_os = "macos")]
#[allow(non_camel_case_types)]
type io_service_t = mach_port_t;

// ... 常量定义保持不变 ...
#[cfg(target_os = "macos")]
const K_IOUSB_DEVICE_CLASS_NAME: &[u8] = b"IOUSBDevice\0";
#[cfg(target_os = "macos")]
const K_USB_VENDOR_ID: &[u8] = b"idVendor\0";
#[cfg(target_os = "macos")]
const K_USB_PRODUCT_ID: &[u8] = b"idProduct\0";
#[cfg(target_os = "macos")]
const K_USB_SERIAL_NUMBER: &[u8] = b"USB Serial Number\0";
#[cfg(target_os = "macos")]
const K_USB_LOCATION_ID: &[u8] = b"locationID\0";
#[cfg(target_os = "macos")]
const K_IO_PUBLISH_NOTIFICATION: &[u8] = b"IOServicePublish\0";
#[cfg(target_os = "macos")]
const K_IO_TERMINATE_NOTIFICATION: &[u8] = b"IOServiceTerminate\0";
#[cfg(target_os = "macos")]
const K_IO_DIALIN_DEVICE: &[u8] = b"IODialinDevice\0";
#[cfg(target_os = "macos")]
const K_IO_CALLOUT_DEVICE: &[u8] = b"IOCalloutDevice\0";

#[cfg(target_os = "macos")]
pub struct MacosMonitor;

#[cfg(target_os = "macos")]
impl MacosMonitor {
    pub fn new() -> Self { Self }

    unsafe fn get_device_path(service: io_service_t) -> String {
        let mut path_buffer: [i8; 512] = [0; 512];
        let plane = CString::new("IOService").unwrap();
        unsafe { IORegistryEntryGetPath(service, plane.as_ptr(), path_buffer.as_mut_ptr()) };
        unsafe { CStr::from_ptr(path_buffer.as_ptr())
            .to_string_lossy()
            .into_owned() }
    }

    /// 执行单次查找
    unsafe fn find_modem_paths_once(device_service: io_service_t) -> (Option<String>, Option<String>) {
        let mut iterator: io_iterator_t = 0;
        let plane_cstr = CString::new("IOService").unwrap();

        let ret = unsafe { IORegistryEntryCreateIterator(
            device_service,
            plane_cstr.as_ptr(),
            1, // kIORegistryIterateRecursively
            &mut iterator
        ) };

        if ret != 0 {
            return (None, None);
        }

        let mut cu_path = None;
        let mut tty_path = None;
        let mut child_service: io_service_t;

        while {
            child_service = unsafe { IOIteratorNext(iterator) };
            child_service != 0
        } {
            if cu_path.is_none() {
                if let Some(path) = unsafe { Self::get_ioreg_string(child_service, K_IO_CALLOUT_DEVICE) } {
                    cu_path = Some(path);
                }
            }
            if tty_path.is_none() {
                if let Some(path) = unsafe { Self::get_ioreg_string(child_service, K_IO_DIALIN_DEVICE) } {
                    tty_path = Some(path);
                }
            }
            if cu_path.is_some() && tty_path.is_some() {
                unsafe { IOObjectRelease(child_service) };
                break;
            }
            unsafe { IOObjectRelease(child_service) };
        }
        unsafe { IOObjectRelease(iterator) };
        (cu_path, tty_path)
    }

    /// 带重试机制的查找
    /// 这里的重试非常关键，因为 USB 驱动加载和 /dev 节点的创建比设备插入事件要滞后
    fn find_modem_paths_with_retry(device_service: io_service_t) -> (Option<String>, Option<String>) {
        // 配置：最多重试 10 次，每次间隔 100ms，总共等待 1秒
        // 对于非串口设备（如键盘），这会引入 1秒 的延迟，但在工业机器人场景下这是可以接受的
        const MAX_RETRIES: usize = 10;
        const RETRY_INTERVAL: u64 = 100;

        unsafe {
            for _ in 0..MAX_RETRIES {
                let (cu, tty) = Self::find_modem_paths_once(device_service);

                // 如果找到了其中任何一个，通常说明驱动已经加载了，我们可以返回
                // (通常 cu 和 tty 是同时出现的，或者至少出现一个)
                if cu.is_some() || tty.is_some() {
                    return (cu, tty);
                }

                // 没找到，睡一会再试
                thread::sleep(Duration::from_millis(RETRY_INTERVAL));
            }

            // 彻底超时，说明这可能不是一个串口设备，或者驱动挂了
            (None, None)
        }
    }

    fn parse_device(service: io_service_t, system_path: String) -> Option<RawDeviceInfo> {
        unsafe {
            let vid = Self::get_ioreg_number(service, K_USB_VENDOR_ID)? as u16;
            let pid = Self::get_ioreg_number(service, K_USB_PRODUCT_ID)? as u16;
            let serial = Self::get_ioreg_string(service, K_USB_SERIAL_NUMBER);

            let location_id = Self::get_ioreg_number(service, K_USB_LOCATION_ID).unwrap_or(0);
            let port_path = format!("0x{:08x}", location_id);

            // 修改点：使用带重试的查找函数
            let (cu, tty) = Self::find_modem_paths_with_retry(service);

            let (final_system_path, system_path_alt) = match (cu, tty) {
                (Some(c), Some(t)) => (c, Some(t)),
                (Some(c), None)    => (c, None),
                (None, Some(t))    => (t, None),
                (None, None)       => (system_path, None)
            };

            Some(RawDeviceInfo {
                vid,
                pid,
                serial,
                port_path,
                system_path: final_system_path,
                system_path_alt,
            })
        }
    }

    // ... 下面的 get_ioreg_number, get_ioreg_string 等辅助函数保持不变 ...
    unsafe fn get_ioreg_number(service: io_service_t, key: &[u8]) -> Option<i32> {
        let key_cf = unsafe { Self::create_cfstring(key) };
        let val_ref = unsafe { IORegistryEntryCreateCFProperty(
            service,
            key_cf,
            kCFAllocatorDefault,
            0
        ) };
        unsafe { CFRelease(key_cf as CFTypeRef) };

        if val_ref.is_null() { return None; }

        let mut value: i32 = 0;
        let success = unsafe { CFNumberGetValue(
            val_ref as CFNumberRef,
            kCFNumberSInt32Type,
            &mut value as *mut _ as *mut c_void
        ) };
        unsafe { CFRelease(val_ref) };

        if success { Some(value) } else { None }
    }

    unsafe fn get_ioreg_string(service: io_service_t, key: &[u8]) -> Option<String> {
        let key_cf = unsafe { Self::create_cfstring(key) };
        let val_ref = unsafe { IORegistryEntryCreateCFProperty(
            service,
            key_cf,
            kCFAllocatorDefault,
            0
        ) };
        unsafe { CFRelease(key_cf as CFTypeRef) };

        if val_ref.is_null() { return None; }

        let mut result = None;
        let cf_str = val_ref as CFStringRef;
        let len = unsafe { CFStringGetLength(cf_str) };
        let max_size = len * 3 + 1;
        let mut buffer = vec![0u8; max_size as usize];

        if unsafe { CFStringGetCString(
            cf_str,
            buffer.as_mut_ptr() as *mut i8,
            max_size,
            kCFStringEncodingUTF8
        ) } != 0 {
            let c_str = unsafe { CStr::from_ptr(buffer.as_ptr() as *const i8) };
            result = Some(c_str.to_string_lossy().into_owned());
        }

        unsafe { CFRelease(val_ref) };
        result
    }

    unsafe fn create_cfstring(bytes: &[u8]) -> CFStringRef {
        let c_str = CStr::from_bytes_with_nul(bytes).unwrap();
        unsafe { CFStringCreateWithCString(kCFAllocatorDefault, c_str.as_ptr(), kCFStringEncodingUTF8) }
    }
}

// ... 结构体 MonitorContext, device_callback, SendableContextPtr 等保持不变 ...
#[cfg(target_os = "macos")]
struct MonitorContext {
    tx: Sender<DeviceEvent>,
    rules: Vec<DeviceRule>,
    active_roles: Mutex<HashMap<String, String>>,
    path_to_role: Mutex<HashMap<String, String>>,
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn device_callback(
    ref_con: *mut c_void,
    iterator: io_iterator_t,
) {
    let ctx = unsafe { &*(ref_con as *const MonitorContext) };
    let mut service: io_service_t;

    while {
        service = unsafe { IOIteratorNext(iterator) };
        service != 0
    } {
        let mut active_roles = ctx.active_roles.lock().unwrap();
        let mut path_to_role = ctx.path_to_role.lock().unwrap();

        let registry_path = unsafe { MacosMonitor::get_device_path(service) };

        if let Some(role) = path_to_role.remove(&registry_path) {
            active_roles.remove(&role);
            ctx.tx.send(DeviceEvent::Detached(role)).ok();
        } else {
            if let Some(dev) = MacosMonitor::parse_device(service, registry_path.clone()) {
                for rule in &ctx.rules {
                    if let Some(method) = rule.matches(&dev) {
                        if !active_roles.contains_key(&rule.role) {
                            active_roles.insert(rule.role.clone(), dev.system_path.clone());
                            path_to_role.insert(registry_path.clone(), rule.role.clone());

                            ctx.tx.send(DeviceEvent::Attached(ResolvedDevice {
                                role: rule.role.clone(),
                                device: dev.clone(),
                                match_method: method,
                            })).ok();
                            break;
                        }
                    }
                }
            }
        }

        unsafe { IOObjectRelease(service) };
    }
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
struct SendableContextPtr(*mut c_void);

#[cfg(target_os = "macos")]
unsafe impl Send for SendableContextPtr {}

#[cfg(target_os = "macos")]
impl DeviceMonitor for MacosMonitor {
    // ... scan_now 保持不变 ...
    fn scan_now(&self) -> Result<Vec<RawDeviceInfo>> {
        unsafe {
            let matching_dict = IOServiceMatching(K_IOUSB_DEVICE_CLASS_NAME.as_ptr() as *const i8);
            let mut iterator: io_iterator_t = 0;

            let ret = IOServiceGetMatchingServices(
                kIOMasterPortDefault,
                matching_dict,
                &mut iterator
            );

            if ret != 0 {
                return Err(anyhow!("Failed to create iterator"));
            }

            let mut devices = Vec::new();
            let mut service: io_service_t;
            while {
                service = IOIteratorNext(iterator);
                service != 0
            } {
                let path = Self::get_device_path(service);
                if let Some(info) = Self::parse_device(service, path) {
                    devices.push(info);
                }
                IOObjectRelease(service);
            }
            IOObjectRelease(iterator);

            Ok(devices)
        }
    }

    // ... start 保持不变 ...
    fn start(&self, rules: Vec<DeviceRule>, tx: Sender<DeviceEvent>) -> Result<()> {
        let initial = self.scan_now()?;
        let active_roles = Mutex::new(HashMap::new());
        let path_to_role = Mutex::new(HashMap::new());

        {
            let mut ar = active_roles.lock().unwrap();
            let mut ptr = path_to_role.lock().unwrap();

            for dev in initial {
                for rule in &rules {
                    if let Some(method) = rule.matches(&dev) {
                        if !ar.contains_key(&rule.role) {
                            ar.insert(rule.role.clone(), dev.system_path.clone());
                            ptr.insert(dev.system_path.clone(), rule.role.clone());
                            tx.send(DeviceEvent::Attached(ResolvedDevice {
                                role: rule.role.clone(),
                                device: dev.clone(),
                                match_method: method,
                            })).ok();
                            break;
                        }
                    }
                }
            }
        }

        let context = Box::new(MonitorContext {
            tx,
            rules,
            active_roles,
            path_to_role,
        });

        let context_ptr = Box::into_raw(context) as *mut c_void;
        let context_addr = context_ptr as usize;

        thread::spawn(move || unsafe {
            let ctx_ptr = context_addr as *mut c_void;

            let notify_port = IONotificationPortCreate(kIOMasterPortDefault);
            let run_loop_source = IONotificationPortGetRunLoopSource(notify_port);
            let run_loop = CFRunLoopGetCurrent();

            CFRunLoopAddSource(
                run_loop,
                run_loop_source,
                kCFRunLoopDefaultMode
            );

            let matching_dict_add = IOServiceMatching(K_IOUSB_DEVICE_CLASS_NAME.as_ptr() as *const i8);
            let mut iter_add: io_iterator_t = 0;
            IOServiceAddMatchingNotification(
                notify_port,
                K_IO_PUBLISH_NOTIFICATION.as_ptr() as *mut i8,
                matching_dict_add,
                device_callback,
                ctx_ptr,
                &mut iter_add
            );
            device_callback(ctx_ptr, iter_add);

            let matching_dict_rem = IOServiceMatching(K_IOUSB_DEVICE_CLASS_NAME.as_ptr() as *const i8);
            let mut iter_rem: io_iterator_t = 0;
            IOServiceAddMatchingNotification(
                notify_port,
                K_IO_TERMINATE_NOTIFICATION.as_ptr() as *mut i8,
                matching_dict_rem,
                device_callback,
                ctx_ptr,
                &mut iter_rem
            );
            device_callback(ctx_ptr, iter_rem);

            CFRunLoopRun();
        });

        Ok(())
    }
}