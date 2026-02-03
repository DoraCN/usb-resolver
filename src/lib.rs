use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};

pub mod platform;

#[cfg(target_os = "linux")]
pub use platform::linux::LinuxMonitor as Monitor;
#[cfg(target_os = "macos")]
pub use platform::macos::MacMonitor as Monitor;
#[cfg(target_os = "windows")]
pub use platform::windows::WindowsMonitor as Monitor;

/// 设备的唯一标识符（由业务层定义，如 "top_camera"）
pub type RoleId = String;

/// 原始设备信息（底层 OS 扫描到的数据）
#[derive(Debug, Clone)]
pub struct RawDeviceInfo {
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    pub port_path: String,               // 平台特定的原生路径字符串
    pub system_path: String,             // 主路径 (macOS 下优先存 /dev/cu.*)
    pub system_path_alt: Option<String>, // 新增：备用路径 (macOS 下存 /dev/tty.*)
}

/// 匹配成功的设备
#[derive(Debug, Clone)]
pub struct ResolvedDevice {
    pub role: RoleId,
    pub device: RawDeviceInfo,
    pub match_method: MatchMethod,
}

#[derive(Debug, Clone, Copy)]
pub enum MatchMethod {
    SerialExact,
    TopologyFallback,
    PortPath,
    VidPidOnly,
}

/// 系统事件
#[derive(Debug, Clone)]
pub enum DeviceEvent {
    /// 一个符合配置要求的设备已上线
    Attached(RawDeviceInfo),
    /// 已知的设备已移除
    Detached(String),
}

/// 单个设备的配置规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRule {
    pub role: RoleId,
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,    // 如果有 Serial，优先匹配
    pub port_path: Option<String>, // 原生路径，用于回退
}

impl DeviceRule {
    /// 核心匹配算法：严格模式
    pub fn matches(&self, device: &RawDeviceInfo) -> Option<MatchMethod> {
        // 1. 基础门槛：VID 和 PID 必须匹配 (Strict Mode)
        if self.vid != device.vid || self.pid != device.pid {
            return None;
        }

        // 2. 策略 A: 序列号匹配 (优先级最高)
        if let (Some(rule_sn), Some(dev_sn)) = (&self.serial, &device.serial)
            && rule_sn == dev_sn
        {
            return Some(MatchMethod::SerialExact);
        }

        // 2. 其次匹配物理路径
        if let Some(rule_path) = &self.port_path
            && rule_path == &device.port_path
        {
            return Some(MatchMethod::PortPath);
        }

        // 3. 如果规则里没写 SN 也没写 Path，则只要 VID/PID 对了就算匹配 (Loose 模式)
        if self.serial.is_none() && self.port_path.is_none() {
            return Some(MatchMethod::VidPidOnly);
        }

        // 3. 策略 B: 拓扑路径匹配 (回退策略)
        // 只有当序列号没匹配上（或配置没写序列号），且配置了路径时才尝试
        if let Some(cfg_path) = &self.port_path
            && cfg_path == &device.port_path
        {
            return Some(MatchMethod::TopologyFallback);
        }

        None
    }
}

/// 统一的监听器 trait
pub trait DeviceMonitor {
    /// 启动监听，阻塞当前线程或在后台运行，通过 channel 发送事件
    fn start(&self, tx: Sender<DeviceEvent>) -> anyhow::Result<()>;

    /// 立即扫描一次当前所有设备（用于程序启动时的初始状态构建）
    fn scan_now(&self) -> anyhow::Result<Vec<RawDeviceInfo>>;
}

/// 工厂方法：获取当前平台的实现
pub fn get_monitor() -> Box<dyn DeviceMonitor> {
    #[cfg(target_os = "linux")]
    return Box::new(platform::linux::LinuxMonitor::new());

    #[cfg(target_os = "windows")]
    return Box::new(platform::windows::WindowsMonitor::new());

    #[cfg(target_os = "macos")]
    return Box::new(platform::macos::MacMonitor::new());

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    panic!("Unsupported OS");
}
