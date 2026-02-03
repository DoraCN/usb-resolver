use std::fs;
use usb_resolver::{DeviceEvent, DeviceRule, get_monitor};

fn main() -> anyhow::Result<()> {
    // 初始化日志
    // env_logger::builder().filter_level(log::LevelFilter::Info).init();

    // println!("=== DORA USB Resolver 启动 ===");

    // // 1. 读取配置
    // // 确保你的 device_config.json 已经根据 discovery 工具填好了十进制的 VID/PID
    // let config_content = fs::read_to_string("device_config.json")
    //     .expect("请先创建 device_config.json 文件");
    // let rules: Vec<DeviceRule> = serde_json::from_str(&config_content)?;
    // println!("已加载 {} 条设备规则", rules.len());

    // // 2. 初始化监控器
    // let monitor = get_monitor();
    // let (tx, rx) = crossbeam_channel::unbounded();

    // // 3. 启动监听
    // println!("启动监控服务...");
    // monitor.start(rules, tx)?;

    // // 4. 模拟业务循环
    // println!("等待设备热插拔事件 (Ctrl+C 退出)...");
    // loop {
    //     match rx.recv() {
    //         Ok(event) => match event {
    //             DeviceEvent::Attached(resolved) => {
    //                 println!("\n[+] 设备上线: {}", resolved.role);

    //                 // --- 这里是修改点：同时显示主路径和备用路径 ---
    //                 if let Some(alt_path) = &resolved.device.system_path_alt {
    //                     println!("    主路径 (Callout): {}", resolved.device.system_path);
    //                     println!("    备路径 (Dialin):  {}", alt_path);
    //                 } else {
    //                     println!("    系统路径: {}", resolved.device.system_path);
    //                 }
    //                 // ------------------------------------------

    //                 println!("    匹配方式: {:?}", resolved.match_method);
    //                 println!("    VID/PID:  {:04} : {:04} (Dec)", resolved.device.vid, resolved.device.pid);

    //                 // 业务逻辑示例：
    //                 // let port_to_open = &resolved.device.system_path; // 优先使用 /dev/cu.xxx
    //                 // start_robot_arm(port_to_open);
    //             },
    //             DeviceEvent::Detached(role) => {
    //                 println!("\n[-] 设备下线: {}", role);
    //                 // stop_robot_arm();
    //             },
    //         },
    //         Err(e) => {
    //             // 修改这里：打印错误信息而不是直接退出
    //             log::error!("监控通道意外关闭，监听线程可能已崩溃: {:?}", e);
    //             eprintln!("❌ 错误: 监控服务停止运行。这通常是因为权限不足或 udev 初始化失败。");
    //             break;
    //         }
    //     }
    // }

    // Ok(())

    let monitor = usb_resolver::get_monitor(); // 会根据系统获取 MacosMonitor

    println!("正在扫描设备...");
    let devices = monitor.scan_now()?;

    for dev in devices {
        println!("{:#?}", dev);
    }

    Ok(())
}
