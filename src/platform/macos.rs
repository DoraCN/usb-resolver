use std::{
    collections::HashSet,
    ffi::{CStr, CString},
    os::raw::c_void,
    sync::Mutex,
    thread,
    time::Duration,
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
use crossbeam_channel::Sender;
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

use crate::{DeviceEvent, DeviceMonitor, RawDeviceInfo};

const IO_USB_DEVICE: &str = "IOUSBDevice";
const IO_SERVICE: &str = "IOService";
const IO_CALLOUT_DEVICE: &str = "IOCalloutDevice";
const IO_DIALIN_DEVICE: &str = "IODialinDevice";
const IO_SERVICE_TERMINATE: &str = "IOServiceTerminate";

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

    // A conversion function that, given a device object and a property name, returns an i32 value.
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

    // A conversion function that, given a device object and property names, returns a Rust String.
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
    fn get_device_path(service: io_service_t) -> String {
        let mut path_buffer = [0i8; 512];

        let plane = CString::new(IO_SERVICE).expect("[get_device_path] Failed to create CString");

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

        let registry_path = Self::get_device_path(service);

        let mut cu = None;
        let mut tty = None;

        let max_retries = 10;
        for _ in 0..max_retries {
            let (c, t) = Self::find_modem_paths(service);

            if c.is_some() || t.is_some() {
                cu = c;
                tty = t;
                break;
            }

            // Couldn't find it. I'll try again after I take a nap.
            // 没找到，睡一会再试
            thread::sleep(Duration::from_millis(100));
        }

        // Strategy: If it's a serial device, prioritize using `cu`. If not (e.g., a keyboard), use `registry_path` as a placeholder.
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
    fn start(&self, tx: Sender<DeviceEvent>) -> Result<()> {
        // Prepare the context
        // We need to perform a scan_now first to populate active_paths with currently existing devices,
        // to avoid receiving duplicate Attach events immediately after startup.
        // 准备上下文 (Context)
        // 我们需要先进行一次 scan_now，把当前已有的设备填入 active_paths，
        // 避免刚启动时重复收到 Attach 事件。
        let context = Box::new(MonitorContext {
            tx,
            activate_paths: Mutex::new(HashSet::new()),
        });

        // Pointer Magic
        // Box::into_raw transfers ownership out of Rust's management, turning it into a raw pointer.
        // This allows us to pass it to C functions or across threads.
        // 指针魔法
        // Box::into_raw 把所有权移出 Rust 管理，变成裸指针。
        // 这样我们就可以把它传给 C 函数，或者跨线程
        let context_prt = Box::into_raw(context) as *mut c_void;

        // Raw pointers are not Send by default and cannot be passed across threads.
        // We cast it to a usize (integer) to trick the compiler, then cast it back inside the thread.
        // 裸指针默认不是 Send 的，不能跨线程。
        // 我们把它强转成 usize (整数) 来欺骗编译器，传到线程里再转回来。
        let context_addr = context_prt as usize;

        // Start the background thread.
        // 启动后台线程
        thread::spawn(move || {
            let ctx_ptr = context_addr as *mut c_void;

            info!("[macOS] Start the IOKit listening thread.");

            // Create port
            // 创建端口
            let notify_port = unsafe { IONotificationPortCreate(kIOMasterPortDefault) };
            let run_loop_source = unsafe { IONotificationPortGetRunLoopSource(notify_port) };
            let run_loop = unsafe { CFRunLoopGetCurrent() };
            unsafe { CFRunLoopAddSource(run_loop, run_loop_source, kCFRunLoopDefaultMode) };

            // Prepare the matching dictionary (Note: Create two copies, as they will be consumed).
            // 准备匹配字典 (注意：要创建两份，因为会被消耗)
            let class_name = CString::new(IO_USB_DEVICE).unwrap();
            let match_ditc_add = unsafe { IOServiceMatching(class_name.as_ptr()) };
            let match_ditc_rem = unsafe { IOServiceMatching(class_name.as_ptr()) };

            // Publish
            // 注册“插入”
            let mut iter_add: io_iterator_t = 0;
            unsafe {
                IOServiceAddMatchingNotification(
                    notify_port,
                    kIOPublishNotification as *mut i8,
                    match_ditc_add as *mut _,
                    device_added_callback,
                    ctx_ptr,
                    &mut iter_add,
                )
            };
            // Activate! (This will automatically send out the Attached event for all currently plugged-in devices.)
            // 激活！(这会自动把当前插着的设备都作为 Attached 事件发出去)
            unsafe { device_added_callback(ctx_ptr, iter_add) };

            // Terminate
            // 注册“拔出”
            let mut iter_rem: io_iterator_t = 0;
            let term_key = CString::new(IO_SERVICE_TERMINATE).unwrap();
            unsafe {
                IOServiceAddMatchingNotification(
                    notify_port,
                    term_key.as_ptr() as *mut i8,
                    match_ditc_rem as *mut _,
                    device_added_callback,
                    ctx_ptr,
                    &mut iter_rem,
                )
            };
            unsafe { device_added_callback(ctx_ptr, iter_rem) };

            // This function initiates an infinite loop, blocking the current thread until the program exits.
            // 启动死循环,这个函数会阻塞当前线程，直到程序退出
            unsafe { CFRunLoopRun() };
        });

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
unsafe extern "C" fn device_added_callback(ref_con: *mut c_void, iterator: io_iterator_t) {
    let mut service: io_service_t;

    // 把裸指针还原成 Rust 引用
    let ctx = unsafe { &*(ref_con as *const MonitorContext) };

    while {
        service = unsafe { IOIteratorNext(iterator) };
        service != 0
    } {
        // 收到一个 USB 设备事件！(Device ID: xxxxxx)
        // info!(
        //     "[Callback], A USB device event has been received! (D evice ID: {})",
        //     service
        // );
        let registry_path = MacMonitor::get_device_path(service);
        let mut activate_paths = ctx.activate_paths.lock().unwrap();
        if activate_paths.contains(&registry_path) {
            activate_paths.remove(&registry_path);

            ctx.tx.send(DeviceEvent::Detached(registry_path)).ok();
        } else {
            if let Some(dev) = MacMonitor::parse_device(service) {
                activate_paths.insert(registry_path);

                ctx.tx.send(DeviceEvent::Attached(dev)).ok();
            }
        }

        // debug
        // let vid = MacMonitor::get_ioreg_number(service, ID_VENDOR);
        // let pid = MacMonitor::get_ioreg_number(service, ID_PRODUCT);
        // let serial = MacMonitor::get_ioreg_string(service, USB_SERIAL_NUMBER);
        // let name = MacMonitor::get_ioreg_string(service, USB_PRODUCT_NAME);
        // if let (Some(v), Some(p)) = (vid, pid) {
        //     info!("   VID: 0x{:04x} ({})", v, v);
        //     info!("   PID: 0x{:04x} ({})", p, p);
        //     info!("   Device: {:?}", name.unwrap_or("Unknown".to_string()));
        //     info!("   Serial: {:?}", serial.unwrap_or("N/A".to_string()));
        // } else {
        //     warn!("   [Warn] 无法读取 VID/PID (可能设备刚拔出)");
        // }

        unsafe { IOObjectRelease(service) };
    }
}

struct MonitorContext {
    tx: Sender<DeviceEvent>,                // Send communication。 发送通信
    activate_paths: Mutex<HashSet<String>>, // Notepad, recording the current device's Registry Path。 记事本，记录当前设备的 Registry Path
}
