use std::{
    ffi::{CStr, CString},
    os::raw::c_void,
};

use anyhow::{Result, anyhow};
use core_foundation::{
    base::{CFRelease, CFTypeRef, kCFAllocatorDefault},
    number::{CFNumberGetValue, CFNumberRef, kCFNumberSInt32Type},
    runloop::{CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRun, kCFRunLoopDefaultMode},
    string::{
        CFStringCreateWithCString, CFStringGetCString, CFStringGetLength, CFStringRef,
        kCFStringEncodingUTF8,
    },
};
use io_kit_sys::{
    IOIteratorNext, IONotificationPortCreate, IONotificationPortGetRunLoopSource,
    IONotificationPortRef, IOObjectRelease, IORegistryEntryCreateCFProperty,
    IORegistryEntryCreateIterator, IORegistryEntryGetPath, IOServiceAddMatchingNotification,
    IOServiceGetMatchingServices, IOServiceMatching, kIOMasterPortDefault,
    kIORegistryIterateRecursively,
    keys::kIOPublishNotification,
    types::{io_iterator_t, io_service_t},
};
use log::{info, warn};

use crate::{DeviceMonitor, RawDeviceInfo};

const IO_USB_DEVICE: &str = "IOUSBDevice";
const IO_SERVICE: &str = "IOService";
const IO_CALLOUT_DEVICE: &str = "IOCalloutDevice";
const IO_DIALIN_DEVICE: &str = "IODialinDevice";
const ID_VENDOR: &str = "idVendor";
const ID_PRODUCT: &str = "idProduct";
const USB_SERIAL_NUMBER: &str = "USB Serial Number";
const USB_PRODUCT_NAME: &str = "USB Product Name";
const LOCATION_ID: &str = "locationID";

// 建立与内核的通信管道
pub struct MacMonitor;

impl Default for MacMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl MacMonitor {
    pub fn new() -> Self {
        Self
    }

    // 开始监听
    pub fn start(&self) {
        // 开始 macOS IOKit 监听
        info!("=== Start macOS IOKit monitoring ===");

        let notify_port = Self::create_notification_port();

        Self::register_usb_listener(notify_port);

        // 准备就绪，按 Ctrl+C 退出，尝试插入 USB 设备...
        info!("=== Ready. Press Ctrl+C to exit. Attempting to insert a USB device... ===");

        unsafe { CFRunLoopRun() };
    }

    /**
     * 核心概念解析，以生活中的例子：
     * Kernel：邮局
     * Notification Port：家门口的邮箱
     * RunLoop Source：是邮箱上的红色小旗子（美式邮箱，有信时旗子会竖起）
     * RunLoop：你自己（坐在门口发呆，没事就喜欢睡觉）
     * CFRunLoopAddSource：只要是红色小旗子竖起来，（Source被触发），我就醒过来取拿信。
     */
    // 第一步：建立连接
    // 返回 IONotificationPortRef
    fn create_notification_port() -> IONotificationPortRef {
        // kIOMasterPortDefault 通常是0，表示默认的主端口
        // 所有的 IOKit 操作都需要通过主端口进行
        let master_port = unsafe { kIOMasterPortDefault };

        // 内核发生事件时候，通过这个端口发送消息
        let notifi_port = unsafe { IONotificationPortCreate(master_port) };
        // 检测这个指针是否为空
        if notifi_port.is_null() {
            // 致命错误：无法创建 IOKit 通知端口，系统可能出现异常
            panic!(
                "Error Unable to create IOKit notification port; the system may be experiencing an error."
            );
        }

        // 通知线程可能会休眠，我们需要一个机制来叫醒通知线程
        // RunLoop Source就是这个机制，当有消息来临的时候，这个 Source 就会被触发
        let run_loop_source = unsafe { IONotificationPortGetRunLoopSource(notifi_port) };
        // 获取当前线程的 RunLoop
        // 注意：如果是在主线程调用，就是主线程的RunLoop；如果是在子线程调用，就是子线程的RunLoop
        let run_loop = unsafe { CFRunLoopGetCurrent() };

        // 消息处理。告诉RunLoop：“嘿伙计，帮我看着点这个 Source，如果它响了，就处理它”
        // kCFRunLoopDefaultMode：程序在正常或者空闲状态下都接收消息
        unsafe { CFRunLoopAddSource(run_loop, run_loop_source, kCFRunLoopDefaultMode) };

        // 管道已铺设完毕，正在监听内核消息...
        info!(
            "[Step 1 create_notification_port Success] The connection with the kernel has been established, and it is now listening for kernel messages..."
        );

        notifi_port
    }

    /**
     * 核心概念解析
     * IOUSBDevice：macOS内核对象层级树中的一个节点类型，当你插入一个USB设备时，内核会生成一个 IOUSBDevice 对象。
     * 这一步就是告诉系统，我要过滤所有的 IOUSBDevice
     */
    // 第二步，开始订阅
    fn create_usb_matching_dictionary() -> *mut c_void {
        let class_name = IO_USB_DEVICE;

        let class_name_c = CString::new(class_name)
            .expect("[create_usb_matching_dictionary] Failed to create CString");

        let matching_dict = unsafe { IOServiceMatching(class_name_c.as_ptr()) };

        if matching_dict.is_null() {
            // 致命错误：无法创建匹配字典 (IOServiceMatching failed)
            panic!("Error Unable to create matching dictionary.(IOServiceMatching failed)");
        }

        // 订阅单已填写：只关注 class_name
        info!(
            "[Step 2 create_usb_matching_dictionary Success] Subscription form has been filled out: Only interested in {}",
            class_name
        );

        matching_dict as *mut c_void
    }

    // 第三步，注册通知
    fn register_usb_listener(notify_port: IONotificationPortRef) {
        let matching_dict = Self::create_usb_matching_dictionary();

        let mut iterator: io_iterator_t = 0;

        let result = unsafe {
            IOServiceAddMatchingNotification(
                notify_port,
                kIOPublishNotification as *mut i8,
                matching_dict as *mut _,
                device_added_callback,
                std::ptr::null_mut(),
                &mut iterator,
            )
        };

        if result != 0 {
            // 注册通知失败，错误码: result
            panic!("Registration notification failed, error code: {}", result);
        }

        // 监听已注册！正在处理现有设备...
        info!(
            "[Step 3 register_usb_listener Success] Listener registered! Processing existing devices."
        );

        unsafe { device_added_callback(std::ptr::null_mut(), iterator) };
    }

    // 转换函数，给定设备对象和属性名，得到i32数值
    fn get_ioreg_number(service: io_service_t, key: &str) -> Option<i32> {
        let key_c = CString::new(key).expect("[get_ioreg_number] Failed to create CString");
        let key_cf = unsafe {
            CFStringCreateWithCString(kCFAllocatorDefault, key_c.as_ptr(), kCFStringEncodingUTF8)
        };

        // 查询数据库
        let val_ref =
            unsafe { IORegistryEntryCreateCFProperty(service, key_cf, kCFAllocatorDefault, 0) };

        // 释放 key_cf
        unsafe { CFRelease(key_cf as CFTypeRef) };

        if val_ref.is_null() {
            return None;
        }

        let mut value: i32 = 0;
        let success = unsafe {
            CFNumberGetValue(
                val_ref as CFNumberRef,
                kCFNumberSInt32Type,
                &mut value as *mut _ as *mut c_void,
            )
        };

        // 释放 val_ref
        unsafe { CFRelease(val_ref) };

        if success { Some(value) } else { None }
    }

    // 转换函数，给定设备对象和属性名，得到 Rust String
    fn get_ioreg_string(service: io_service_t, key: &str) -> Option<String> {
        let key_c = CString::new(key).expect("[get_ioreg_number] Failed to create CString");
        let key_cf = unsafe {
            CFStringCreateWithCString(kCFAllocatorDefault, key_c.as_ptr(), kCFStringEncodingUTF8)
        };

        // 查询数据库
        let val_ref =
            unsafe { IORegistryEntryCreateCFProperty(service, key_cf, kCFAllocatorDefault, 0) };

        // 释放 key_cf
        unsafe { CFRelease(key_cf as CFTypeRef) };

        if val_ref.is_null() {
            return None;
        }

        // 转换字符串
        let cf_str = val_ref as CFStringRef;
        let len = unsafe { CFStringGetLength(cf_str) };
        // 分配缓冲区： 长度 * 3（防止UTF8多字节） + 1 （结尾\0）
        let max_size = len * 3 + 1;
        let mut buffer = vec![0u8; max_size as usize];

        let mut result = None;
        if unsafe {
            CFStringGetCString(
                cf_str,
                buffer.as_mut_ptr() as *mut i8,
                max_size,
                kCFStringEncodingUTF8,
            )
        } != 0
        {
            let c_str = unsafe { CStr::from_ptr(buffer.as_ptr() as *const i8) };
            result = Some(c_str.to_string_lossy().into_owned());
        }

        unsafe { CFRelease(val_ref) };

        result
    }

    /**
     * 在 macOS 插上一个 USB 转串口芯片（比如 Arduino 或 Lidar）时，内核里会生成这样一颗树：
     * IOUSBDevice (物理设备, 我们在这)
     * |
     * +-- IOUSBInterface (接口层)
     *      |
     *      +-- AppleUSBCDCACMData (驱动层)
     *           |
     *           +-- IOCalloutDevice (这才是 /dev/cu!)
     *           +-- IODialinDevice (这才是 /dev/tty!)
     */
    // 递归查找 TTY 设备路径
    // 返回两个数据：
    // IOCalloutDevice -> /dev/cu.* (主要用这个，Open 时不阻塞)
    // IODialinDevice -> /dev/tty.* (备用)
    fn find_modem_paths(device_service: io_service_t) -> (Option<String>, Option<String>) {
        let mut iterator: io_iterator_t = 0;

        let plane_cstr =
            CString::new(IO_SERVICE).expect("[find_modem_paths] Failed to create CString");

        let ret = unsafe {
            IORegistryEntryCreateIterator(
                device_service,
                plane_cstr.as_ptr(),
                kIORegistryIterateRecursively,
                &mut iterator,
            )
        };

        let mut cu_path = None;
        let mut tty_path = None;

        if ret == 0 {
            let mut child_service: io_service_t;

            while {
                child_service = unsafe { IOIteratorNext(iterator) };
                child_service != 0
            } {
                if cu_path.is_none() {
                    cu_path = Self::get_ioreg_string(child_service, IO_CALLOUT_DEVICE);
                }

                if tty_path.is_none() {
                    tty_path = Self::get_ioreg_string(child_service, IO_DIALIN_DEVICE);
                }

                unsafe { IOObjectRelease(child_service) };

                if cu_path.is_some() && tty_path.is_some() {
                    break;
                }
            }
        }

        unsafe { IOObjectRelease(iterator) };

        (cu_path, tty_path)
    }

    // 生成设备唯一ID
    fn get_device_id(service: io_service_t) -> String {
        let mut path_buffer = [0i8; 512];

        let plane = CString::new(IO_SERVICE).expect("[get_device_id] Failed to create CString");

        unsafe { IORegistryEntryGetPath(service, plane.as_ptr(), path_buffer.as_mut_ptr()) };

        unsafe { CStr::from_ptr(path_buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    // 整合
    fn parse_device(service: io_service_t) -> Option<RawDeviceInfo> {
        let vid = Self::get_ioreg_number(service, ID_VENDOR)? as u16;
        let pid = Self::get_ioreg_number(service, ID_PRODUCT)? as u16;
        let serial = Self::get_ioreg_string(service, USB_SERIAL_NUMBER);

        let location_id = Self::get_ioreg_number(service, LOCATION_ID).unwrap_or_default();
        let port_path = format!("0x{:08x}", location_id);

        let registry_path = Self::get_device_id(service);

        let (cu, tty) = Self::find_modem_paths(service);

        // 策略：如果是串口设备，优先用 cu。如果不是(比如键盘)，用 registry_path 占位。
        let (system_path, system_path_alt) = match (cu, tty) {
            (Some(c), Some(t)) => (c, Some(t)),    // 完美：都有
            (Some(c), None) => (c, None),          // 只有 cu
            (None, Some(t)) => (t, None),          // 只有 tty
            (None, None) => (registry_path, None), // 不是串口设备
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

impl DeviceMonitor for MacMonitor {
    fn start(
        &self,
        rules: Vec<crate::DeviceRule>,
        event_sender: crossbeam_channel::Sender<crate::DeviceEvent>,
    ) -> Result<()> {
        Ok(())
    }

    fn scan_now(&self) -> Result<Vec<RawDeviceInfo>> {
        let class_name =
            CString::new(IO_USB_DEVICE).expect("[IO_USB_DEVICE] Failed to create CString");
        let matching_dict = unsafe { IOServiceMatching(class_name.as_ptr()) };

        if matching_dict.is_null() {
            return Err(anyhow!("Failed to create matching dictionary"));
        }

        let mut iterator: io_iterator_t = 0;

        let ret = unsafe {
            IOServiceGetMatchingServices(kIOMasterPortDefault, matching_dict, &mut iterator)
        };

        if ret != 0 {
            return Err(anyhow!(
                "IOServiceGetMatchingServices failed with code: {}",
                ret
            ));
        }

        let mut devices = vec![];
        let mut service: io_service_t;

        while {
            service = unsafe { IOIteratorNext(iterator) };
            service != 0
        } {
            if let Some(info) = Self::parse_device(service) {
                devices.push(info);
            }

            unsafe { IOObjectRelease(service) };
        }

        unsafe { IOObjectRelease(iterator) };

        Ok(devices)
    }
}

// 定义回调函数
unsafe extern "C" fn device_added_callback(_ref_con: *mut c_void, iterator: io_iterator_t) {
    let mut device: io_service_t;

    while {
        device = unsafe { IOIteratorNext(iterator) };
        device != 0
    } {
        // 收到一个 USB 设备事件！(Device ID: xxxxxx)
        info!(
            "[Callback], A USB device event has been received! (Device ID: {})",
            device
        );

        let vid = MacMonitor::get_ioreg_number(device, ID_VENDOR);
        let pid = MacMonitor::get_ioreg_number(device, ID_PRODUCT);
        let serial = MacMonitor::get_ioreg_string(device, USB_SERIAL_NUMBER);
        let name = MacMonitor::get_ioreg_string(device, USB_PRODUCT_NAME);

        if let (Some(v), Some(p)) = (vid, pid) {
            info!("   VID: 0x{:04x} ({})", v, v);
            info!("   PID: 0x{:04x} ({})", p, p);
            info!("   Device: {:?}", name.unwrap_or("Unknown".to_string()));
            info!("   Serial: {:?}", serial.unwrap_or("N/A".to_string()));
        } else {
            warn!("   [Warn] 无法读取 VID/PID (可能设备刚拔出)");
        }

        unsafe { IOObjectRelease(device) };
    }
}
