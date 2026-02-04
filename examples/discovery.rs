use usb_resolver::get_monitor;

fn main() -> anyhow::Result<()> {
    env_logger::try_init().ok();

    let monitor = get_monitor();
    println!("正在扫描系统 USB 设备...\n");

    let devices = monitor.scan_now()?;

    if devices.is_empty() {
        println!("未发现 USB 设备。");
        return Ok(());
    }

    // 表头：区分 Hex (用于核对) 和 Dec (用于 JSON 配置)
    println!(
        "{:<10} | {:<10} | {:<10} | {:<10} | {:<20} | {:<25} | Path",
        "VID(Hex)", "VID(Dec)", "PID(Hex)", "PID(Dec)", "Serial", "Port Path"
    );
    println!("{}", "-".repeat(130));

    for dev in devices {
        println!(
            "0x{:<04x}     | {:<10} | 0x{:<04x}     | {:<10} | {:<20} | {:<25} | {}",
            dev.vid, // Hex 显示
            dev.vid, // Dec 显示 (复制这个到 JSON)
            dev.pid, // Hex 显示
            dev.pid, // Dec 显示 (复制这个到 JSON)
            dev.serial.as_deref().unwrap_or("N/A"),
            dev.port_path,
            dev.system_path
        );
    }

    println!("[配置指南]");
    println!("JSON 配置文件不支持十六进制。");
    println!("请复制表格中 'VID(Dec)' 和 'PID(Dec)' 列的【十进制数字】到 device_config.json 中。");
    println!("例如: 如果 VID(Hex) 是 0x3290，请在 JSON 中填 12944。");

    Ok(())
}
