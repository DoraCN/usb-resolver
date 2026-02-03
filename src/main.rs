use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{prelude::*, widgets::*};
use std::fs;
use std::{collections::HashMap, io, time::Duration};
use usb_resolver::{DeviceEvent, DeviceRule, RawDeviceInfo, get_monitor};

// --- 状态管理 ---
struct App {
    // 原始数据源 (Key = Registry Path)
    devices_map: HashMap<String, RawDeviceInfo>,

    // 排序后的列表 (用于 UI 显示和索引选择)
    sorted_devices: Vec<RawDeviceInfo>,

    // 表格的选择状态
    table_state: TableState,

    // 配置文件里的规则
    rules: Vec<DeviceRule>,

    // 弹窗状态：如果为 Some，则显示该设备的详情
    popup_device: Option<RawDeviceInfo>,
}

impl App {
    fn new(rules: Vec<DeviceRule>) -> Self {
        let mut state = TableState::default();
        state.select(Some(0)); // 默认选中第一行

        Self {
            devices_map: HashMap::new(),
            sorted_devices: Vec::new(),
            table_state: state,
            rules,
            popup_device: None,
        }
    }

    // 当设备列表变动时，重新生成排序列表，保证光标位置正确
    fn refresh_list(&mut self) {
        let mut list: Vec<RawDeviceInfo> = self.devices_map.values().cloned().collect();
        // 按 system_path 排序，保证列表稳定性
        list.sort_by(|a, b| a.system_path.cmp(&b.system_path));
        self.sorted_devices = list;
    }

    fn match_role(&self, info: &RawDeviceInfo) -> String {
        for rule in &self.rules {
            if rule.matches(info).is_some() {
                return rule.role.clone();
            }
        }
        "-".to_string()
    }

    // --- 导航逻辑 ---
    fn next(&mut self) {
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= self.sorted_devices.len().saturating_sub(1) {
                    0 // 回到顶部
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn previous(&mut self) {
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.sorted_devices.len().saturating_sub(1) //以此到底部
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn open_popup(&mut self) {
        if let Some(i) = self.table_state.selected() {
            if let Some(dev) = self.sorted_devices.get(i) {
                self.popup_device = Some(dev.clone());
            }
        }
    }

    fn close_popup(&mut self) {
        self.popup_device = None;
    }
}

fn main() -> Result<()> {
    // 加载配置
    let rules = if let Ok(content) = fs::read_to_string("device_config.json") {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };

    // 启动 Monitor
    let monitor = get_monitor();
    let (tx, rx) = crossbeam_channel::unbounded();
    monitor.start(tx)?;

    // TUI Setup
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(rules);

    // Run Loop
    let res = run_app(&mut terminal, &mut app, &rx);

    // Cleanup
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    rx: &crossbeam_channel::Receiver<DeviceEvent>,
) -> anyhow::Result<()> {
    loop {
        // --- Draw ---
        terminal
            .draw(|f| ui(f, app))
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        // --- Handle USB Events ---
        let mut need_refresh = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                DeviceEvent::Attached(dev) => {
                    app.devices_map.insert(dev.system_path.clone(), dev);
                    need_refresh = true;
                }
                DeviceEvent::Detached(path) => {
                    app.devices_map.remove(&path);
                    need_refresh = true;
                }
            }
        }
        if need_refresh {
            app.refresh_list();
        }

        // --- Handle Keyboard ---
        if crossterm::event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // 如果弹窗打开了，只响应 Esc 和 Enter(关闭)
                if app.popup_device.is_some() {
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => app.close_popup(),
                        _ => {}
                    }
                } else {
                    // 弹窗没打开，响应导航
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Down | KeyCode::Char('j') => app.next(),
                        KeyCode::Up | KeyCode::Char('k') => app.previous(),
                        KeyCode::Enter => app.open_popup(),
                        _ => {}
                    }
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(f.area());

    // Title
    let title = Paragraph::new("🔌 DORA USB Resolver")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    // Table
    let header = Row::new(vec!["Role", "VID", "PID", "Serial", "System Path"])
        .style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .height(1)
        .bottom_margin(1);

    let rows: Vec<Row> = app
        .sorted_devices
        .iter()
        .map(|item| {
            let role = app.match_role(item);
            let style = if role != "-" {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };

            let display_path = if let Some(alt) = &item.system_path_alt {
                format!("{} ({})", alt, item.system_path)
            } else {
                item.system_path.clone()
            };

            Row::new(vec![
                Cell::from(role),
                Cell::from(format!("0x{:04x}", item.vid)),
                Cell::from(format!("0x{:04x}", item.pid)),
                Cell::from(item.serial.clone().unwrap_or_else(|| "N/A".to_string())),
                Cell::from(display_path),
            ])
            .style(style)
            .height(1)
        })
        .collect();

    let t = Table::new(
        rows,
        [
            Constraint::Percentage(15),
            Constraint::Percentage(10),
            Constraint::Percentage(10),
            Constraint::Percentage(15),
            Constraint::Percentage(50),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Device List "),
    )
    // 选中行的样式：黄色背景，黑色文字
    .row_highlight_style(Style::default().bg(Color::Yellow).fg(Color::Black))
    .highlight_symbol(">> ");

    // 使用 render_stateful_widget 来支持选中状态
    f.render_stateful_widget(t, chunks[1], &mut app.table_state);

    // Footer
    let help_text = if app.popup_device.is_some() {
        "ESC: Close Popup"
    } else {
        "↑/↓: Select | Enter: Details | q: Quit"
    };
    let footer = Paragraph::new(format!(
        "Total: {} | {}",
        app.sorted_devices.len(),
        help_text
    ))
    .style(Style::default().fg(Color::Gray))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, chunks[2]);

    // Render Popup
    if let Some(dev) = &app.popup_device {
        render_popup(f, dev, app);
    }
}

// 渲染详细信息弹窗
fn render_popup(f: &mut Frame, dev: &RawDeviceInfo, app: &App) {
    let area = centered_rect(60, 50, f.area());

    // 清除背景 (否则表格的内容会透出来)
    f.render_widget(Clear, area);

    let role = app.match_role(dev);

    // 准备详细信息文本
    let text = vec![
        Line::from(vec![
            Span::styled("Role: ", Style::default().fg(Color::Yellow)),
            Span::raw(role),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("VID (Hex): ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("0x{:04x}", dev.vid)),
        ]),
        Line::from(vec![
            Span::styled("VID (Dec): ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}", dev.vid)),
        ]),
        Line::from(vec![
            Span::styled("PID (Hex): ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("0x{:04x}", dev.pid)),
        ]),
        Line::from(vec![
            Span::styled("PID (Dec): ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}", dev.pid)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Serial: ", Style::default().fg(Color::Magenta)),
            Span::raw(dev.serial.clone().unwrap_or("N/A".to_string())),
        ]),
        Line::from(vec![
            Span::styled("Port Path: ", Style::default().fg(Color::Magenta)),
            Span::raw(&dev.port_path),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "--- Paths ---",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("Primary (ID): ", Style::default().fg(Color::Green)),
            Span::raw(&dev.system_path),
        ]),
        Line::from(vec![
            Span::styled("Alt (Usable): ", Style::default().fg(Color::Green)),
            Span::raw(dev.system_path_alt.clone().unwrap_or("N/A".to_string())),
        ]),
    ];

    let block = Block::default()
        .title(" Device Details (Press ESC to close) ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::DarkGray)); // 弹窗稍微深色一点

    let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true });

    f.render_widget(p, area);
}

// 辅助函数：计算屏幕中间的矩形
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
