use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ddc_core::{Brightness, MonitorInfo, enumerate, set_brightness};
use serde_json::json;

const USAGE: &str = "\
ddcctl — LumenTray DDC/CI 诊断命令行

用法:
  ddcctl list [--json] [--time]
  ddcctl get <目标> [--json] [--time]
  ddcctl set <目标> <0-100> [--json] [--time]
  ddcctl report [--json] [--time]
  ddcctl bench [--json] [--time]   同值连写 5 次，测枚举与稳态写入耗时

目标可以是: 列表序号(0,1,...)、GDI 名(DISPLAY1)、显示器 ID、
型号关键字(U2412M)、primary 或 GDI#物理屏序号(DISPLAY1#1)。

退出码: 0 成功, 1 DDC/CI 失败, 2 参数或目标错误。";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let json = take_flag(&mut args, "--json");
    let time = take_flag(&mut args, "--time");

    let started = Instant::now();
    let Some(command) = args.first().cloned() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    args.remove(0);

    let code = match command.as_str() {
        "list" => cmd_list(json, started),
        "get" => cmd_get(&args, json),
        "set" => cmd_set(&args, json),
        "report" => cmd_report(json, started),
        "bench" => cmd_bench(json),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            0
        }
        other => {
            eprintln!("未知命令: {other}\n\n{USAGE}");
            2
        }
    };

    if time {
        eprintln!("elapsed: {} ms", started.elapsed().as_millis());
    }
    ExitCode::from(code)
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(position) = args.iter().position(|arg| arg == flag) {
        args.remove(position);
        true
    } else {
        false
    }
}

fn cmd_list(json: bool, started: Instant) -> u8 {
    let list = enumerate();
    if json {
        let payload = json!({
            "monitors": list.monitors,
            "notes": list.notes,
            "elapsedMs": started.elapsed().as_millis() as u64,
        });
        print_json(&payload);
    } else {
        print_table(&list.monitors);
        print_notes(&list.notes);
    }
    0
}

fn cmd_get(args: &[String], json: bool) -> u8 {
    let Some(target) = args.first() else {
        eprintln!("用法: ddcctl get <目标> [--json]");
        return 2;
    };
    let list = enumerate();
    let monitor = match resolve(&list.monitors, target) {
        Ok(monitor) => monitor,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    let brightness = monitor.brightness;
    if json {
        print_json(&json!({
            "target": target,
            "monitor": monitor,
            "brightness": brightness,
        }));
    } else if let Some(brightness) = brightness {
        println!("{}: {}", label(monitor), format_brightness(&brightness));
    } else {
        let code = monitor.error.as_ref().map(|error| error.code).unwrap_or(0);
        println!(
            "{}: 读取失败 0x{code:08X} {}",
            label(monitor),
            ddc_core::error_name(code)
        );
    }
    print_notes(&list.notes);
    if brightness.is_some() { 0 } else { 1 }
}

fn cmd_set(args: &[String], json: bool) -> u8 {
    let (Some(target), Some(percent_text)) = (args.first(), args.get(1)) else {
        eprintln!("用法: ddcctl set <目标> <0-100> [--json]");
        return 2;
    };
    let Ok(percent) = percent_text.parse::<u32>() else {
        eprintln!("亮度必须是 0-100 的整数: {percent_text}");
        return 2;
    };
    if percent > 100 {
        eprintln!("亮度必须在 0-100 之间: {percent}");
        return 2;
    }

    let list = enumerate();
    let monitor = match resolve(&list.monitors, target) {
        Ok(monitor) => monitor,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    let id = monitor.id.clone();
    let name = label(monitor);
    let outcome = set_brightness(&id, percent);

    if json {
        print_json(&json!({
            "target": target,
            "id": id,
            "requested": percent,
            "outcome": outcome,
        }));
    } else if let Some(error) = &outcome.error {
        println!(
            "{name}: 写入 {percent}% 失败 0x{:08X} {} ({})",
            error.code,
            error.name(),
            error.context
        );
    } else if let Some(brightness) = &outcome.brightness {
        println!(
            "{name}: 写入 {percent}%，回读 {}",
            format_brightness(brightness)
        );
    } else {
        println!("{name}: 写入 {percent}%");
    }
    print_notes(&list.notes);
    if outcome.error.is_some() { 1 } else { 0 }
}

fn cmd_report(json: bool, started: Instant) -> u8 {
    let list = enumerate();
    if json {
        let generated = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let payload = json!({
            "tool": "ddcctl",
            "version": env!("CARGO_PKG_VERSION"),
            "generatedAtUnix": generated,
            "monitors": list.monitors,
            "notes": list.notes,
            "elapsedMs": started.elapsed().as_millis() as u64,
        });
        print_json(&payload);
        return 0;
    }

    println!(
        "ddcctl {} · {} 块显示器",
        env!("CARGO_PKG_VERSION"),
        list.monitors.len()
    );
    for (index, monitor) in list.monitors.iter().enumerate() {
        println!("\n[{index}] {}", label(monitor));
        println!("    id:          {}", monitor.id);
        if !monitor.gdi_name.is_empty() {
            println!("    gdi:         {}", monitor.gdi_name);
        }
        if !monitor.description.is_empty() {
            println!("    description: {}", monitor.description);
        }
        if !monitor.device_path.is_empty() {
            println!("    device:      {}", monitor.device_path);
        }
        println!(
            "    link:        {}{}",
            if monitor.connection.is_empty() {
                "—"
            } else {
                &monitor.connection
            },
            if monitor.primary { "（主屏）" } else { "" }
        );
        match &monitor.brightness {
            Some(brightness) => {
                println!("    brightness:  {}", format_brightness(brightness))
            }
            None => {
                let code = monitor.error.as_ref().map(|error| error.code).unwrap_or(0);
                println!(
                    "    brightness:  读取失败 0x{code:08X} {}",
                    ddc_core::error_name(code)
                );
            }
        }
        if let Some(last) = monitor.last_percent {
            println!("    lastKnown:   {last}%");
        }
        if let Some(error) = &monitor.error {
            println!("    error:       {} ({})", error.context, error.name());
        }
    }
    print_notes(&list.notes);
    0
}

fn cmd_bench(json: bool) -> u8 {
    let enumerate_started = Instant::now();
    let list = enumerate();
    let enumerate_ms = enumerate_started.elapsed().as_millis() as u64;
    let Some(monitor) = list
        .monitors
        .iter()
        .find(|monitor| monitor.brightness.is_some())
    else {
        eprintln!("没有可写的显示器");
        return 1;
    };
    let percent = monitor
        .brightness
        .map(|brightness| brightness.percent)
        .unwrap_or(50);
    let id = monitor.id.clone();
    let name = label(monitor);

    let mut writes = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        let outcome = set_brightness(&id, percent);
        if let Some(error) = outcome.error {
            eprintln!(
                "{name}: 写入失败 0x{:08X} {} ({})",
                error.code,
                error.name(),
                error.context
            );
            return 1;
        }
        writes.push(started.elapsed().as_millis() as u64);
    }
    let average = writes.iter().sum::<u64>() / writes.len() as u64;

    if json {
        print_json(&json!({
            "monitor": name,
            "percent": percent,
            "enumerateMs": enumerate_ms,
            "writeMs": writes,
            "writeAverageMs": average,
        }));
    } else {
        println!("{name} @ {percent}%");
        println!("enumerate: {enumerate_ms} ms");
        println!("writes:    {writes:?} ms (平均 {average} ms)");
    }
    0
}

fn print_table(monitors: &[MonitorInfo]) {
    if monitors.is_empty() {
        println!("没有检测到可控制的显示器。");
        return;
    }
    println!(
        "{:<3} {:<10} {:<22} {:<6} BRIGHTNESS",
        "#", "GDI", "NAME", "LINK"
    );
    for (index, monitor) in monitors.iter().enumerate() {
        let name = truncate(&display_name(monitor), 22);
        let link = if monitor.connection.is_empty() {
            "—".to_string()
        } else {
            monitor.connection.clone()
        };
        let state = match &monitor.brightness {
            Some(brightness) => format_brightness(brightness),
            None => {
                let code = monitor.error.as_ref().map(|error| error.code).unwrap_or(0);
                format!("无响应 0x{code:08X}")
            }
        };
        println!(
            "{index:<3} {:<10} {name:<22} {link:<6} {state}",
            short_gdi(&monitor.gdi_name)
        );
    }
}

fn print_notes(notes: &[String]) {
    if notes.is_empty() {
        return;
    }
    println!("\n枚举备注:");
    for note in notes {
        println!("  - {note}");
    }
}

fn print_json(value: &serde_json::Value) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(error) => eprintln!("序列化失败: {error}"),
    }
}

fn display_name(monitor: &MonitorInfo) -> String {
    if !monitor.name.is_empty() {
        monitor.name.clone()
    } else if !monitor.description.is_empty() {
        monitor.description.clone()
    } else {
        monitor.gdi_name.clone()
    }
}

fn label(monitor: &MonitorInfo) -> String {
    if monitor.gdi_name.is_empty() {
        display_name(monitor)
    } else {
        format!("{} {}", short_gdi(&monitor.gdi_name), display_name(monitor))
    }
}

fn short_gdi(gdi_name: &str) -> &str {
    gdi_name.strip_prefix(r"\\.\").unwrap_or(gdi_name)
}

fn format_brightness(brightness: &Brightness) -> String {
    let method = match brightness.method {
        ddc_core::BrightnessMethod::HighLevel => "high-level",
        ddc_core::BrightnessMethod::Vcp => "VCP 0x10",
    };
    format!(
        "{}% (raw {}/{}, {method})",
        brightness.percent, brightness.raw, brightness.max
    )
}

fn resolve<'a>(monitors: &'a [MonitorInfo], target: &str) -> Result<&'a MonitorInfo, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("目标不能为空".into());
    }
    if monitors.is_empty() {
        return Err("没有检测到可控制的显示器".into());
    }

    if target.eq_ignore_ascii_case("primary") {
        return monitors
            .iter()
            .find(|monitor| monitor.primary)
            .ok_or_else(|| "没有找到主屏".to_string());
    }

    if let Ok(index) = target.parse::<usize>()
        && index < monitors.len()
    {
        return Ok(&monitors[index]);
    }

    if let Some(monitor) = monitors
        .iter()
        .find(|monitor| monitor.id.eq_ignore_ascii_case(target))
    {
        return Ok(monitor);
    }

    let (gdi, physical) = split_physical(target);
    let gdi_name = normalize_gdi(&gdi);
    if let Some(physical) = physical {
        return monitors
            .iter()
            .find(|monitor| {
                monitor.gdi_name.eq_ignore_ascii_case(&gdi_name)
                    && monitor.id.ends_with(&format!("#{physical}"))
            })
            .ok_or_else(|| format!("没有找到 {gdi_name}#{physical}"));
    }

    let gdi_matches: Vec<&MonitorInfo> = monitors
        .iter()
        .filter(|monitor| monitor.gdi_name.eq_ignore_ascii_case(&gdi_name))
        .collect();
    if gdi_matches.len() == 1 {
        return Ok(gdi_matches[0]);
    }
    if gdi_matches.len() > 1 {
        return Err(format!(
            "{gdi_name} 下有多个物理屏，请加序号: {}",
            gdi_matches
                .iter()
                .map(|monitor| short_id(monitor))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let needle = target.to_lowercase();
    let name_matches: Vec<&MonitorInfo> = monitors
        .iter()
        .filter(|monitor| {
            monitor.name.to_lowercase().contains(&needle)
                || monitor.description.to_lowercase().contains(&needle)
        })
        .collect();
    match name_matches.len() {
        1 => Ok(name_matches[0]),
        0 => Err(format!("没有找到匹配 “{target}” 的显示器")),
        _ => Err(format!(
            "“{target}” 匹配到多块显示器，请用序号或 GDI: {}",
            name_matches
                .iter()
                .enumerate()
                .map(|(index, monitor)| format!("{index} {}", label(monitor)))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn short_id(monitor: &MonitorInfo) -> String {
    monitor
        .id
        .rsplit_once('#')
        .map(|(_, index)| format!("{}#{index}", monitor.gdi_name))
        .unwrap_or_else(|| monitor.gdi_name.clone())
}

fn split_physical(target: &str) -> (String, Option<u32>) {
    if let Some((base, suffix)) = target.rsplit_once('#')
        && let Ok(index) = suffix.parse::<u32>()
    {
        return (base.to_string(), Some(index));
    }
    (target.to_string(), None)
}

fn normalize_gdi(target: &str) -> String {
    let upper = target.to_ascii_uppercase();
    if upper.starts_with(r"\\.\") {
        upper
    } else {
        format!(r"\\.\{upper}")
    }
}

fn truncate(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_string();
    }
    let mut result: String = chars[..width.saturating_sub(1)].iter().collect();
    result.push('…');
    result
}
