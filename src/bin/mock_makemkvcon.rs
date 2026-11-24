//! Stand-in for makemkvcon used by the integration test. Replays the real
//! Kare Kano Vol. 5 scan (tests/fixtures) on MOCK_DRIVES drives and mimics
//! the behaviour of the real MakeMKV 2.0.0 that the app depends on:
//!   - without --minlength=0, titles under 120 s are dropped before numbering
//!   - without --progress=-same, no PRGV/PRGC lines are printed
//!
//! Environment:
//!   MOCK_FIXTURE      path of the recorded scan
//!   MOCK_DRIVES       number of drives (default 1)
//!   MOCK_DISC_FLAG    drive N has a disc while the file "<flag>.N" exists
//!   MOCK_FAIL_FLAG    while this file exists every rip fails
//!   MOCK_SCAN_DELAY   seconds an "info" scan takes (default 0)
//!   MOCK_RIP_STEP_MS  delay between progress lines of a rip (default 30)

use std::path::Path;
use std::time::Duration;

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_default()
}

fn disc_present(drive: usize) -> bool {
    let flag = env("MOCK_DISC_FLAG");
    flag.is_empty() || Path::new(&format!("{flag}.{drive}")).exists()
}

fn disc_label(drive: usize) -> String {
    if drive == 0 {
        "KARE_KANO_VOL5".into()
    } else {
        format!("KARE_KANO_VOL{}", 5 + drive)
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&String> = argv.iter().filter(|a| !a.starts_with('-')).collect();
    let minlength0 = argv.iter().any(|a| a == "--minlength=0");
    let progress = argv.iter().any(|a| a == "--progress=-same");
    let fixture = std::fs::read_to_string(env("MOCK_FIXTURE")).expect("MOCK_FIXTURE");
    let lines: Vec<&str> = fixture.lines().collect();
    let drives: usize = env("MOCK_DRIVES").parse().unwrap_or(1);

    println!(r#"MSG:1005,0,1,"MakeMKV v2.0.0 linux(x64-release) started","%1 started","MakeMKV v2.0.0 linux(x64-release)""#);
    for i in 0..16 {
        if i < drives {
            let label = if disc_present(i) { disc_label(i) } else { String::new() };
            println!(r#"DRV:{i},2,999,1,"BD-RE MOCK DRIVE{i} 1.00 SERIAL{i}","{label}","/dev/sr{i}""#);
        } else {
            println!(r#"DRV:{i},256,999,0,"","","""#);
        }
    }

    let (Some(cmd), Some(source)) = (args.first(), args.get(1)) else { std::process::exit(1) };
    let drive: Option<usize> = source.strip_prefix("disc:").and_then(|d| d.parse().ok()).filter(|d| *d < drives);
    let Some(drive) = drive.filter(|d| disc_present(*d)) else {
        println!(r#"MSG:5010,0,0,"Failed to open disc","Failed to open disc""#);
        return;
    };

    // Title durations in the recording, to emulate --minlength.
    let mut durations: Vec<(usize, i64)> = Vec::new();
    for l in &lines {
        if let Some(rest) = l.strip_prefix("TINFO:") {
            let parts: Vec<&str> = rest.splitn(4, ',').collect();
            if parts.len() == 4 && parts[1] == "9" {
                let hms: Vec<i64> = parts[3].trim_matches('"').split(':').filter_map(|x| x.parse().ok()).collect();
                if let [h, m, s] = hms[..] {
                    durations.push((parts[0].parse().unwrap(), h * 3600 + m * 60 + s));
                }
            }
        }
    }
    let kept: Vec<usize> = durations.iter().filter(|(_, d)| minlength0 || *d >= 120).map(|(i, _)| *i).collect();

    if cmd.as_str() == "info" {
        let delay: f64 = env("MOCK_SCAN_DELAY").parse().unwrap_or(0.0);
        std::thread::sleep(Duration::from_secs_f64(delay));
        for l in &lines {
            if let Some(rest) = l.strip_prefix("TINFO:") {
                let (idx, tail) = rest.split_once(',').unwrap();
                if let Some(pos) = kept.iter().position(|k| *k == idx.parse::<usize>().unwrap()) {
                    println!("TINFO:{pos},{tail}");
                }
            } else if l.starts_with("CINFO:2,") {
                println!(r#"CINFO:2,0,"{}""#, disc_label(drive));
            } else if l.starts_with("CINFO:") || l.starts_with("TCOUNT:") {
                println!("{l}");
            }
        }
        return;
    }

    if cmd.as_str() == "mkv" {
        let (Some(title), Some(out_dir)) = (args.get(2), args.get(3)) else { std::process::exit(1) };
        let title: usize = title.parse().unwrap();
        let Some(&real) = kept.get(title) else {
            println!(r#"MSG:5016,0,0,"Title #{title} not found","Title #%1 not found""#);
            std::process::exit(12);
        };
        let step: u64 = env("MOCK_RIP_STEP_MS").parse().unwrap_or(30);
        if progress {
            println!(r#"PRGT:5024,0,"Saving all titles to MKV files""#);
            println!(r#"PRGC:5017,0,"Saving to MKV file""#);
            for s in (0..=65536).step_by(16384) {
                println!("PRGV:{s},{s},65536");
                std::thread::sleep(Duration::from_millis(step));
            }
        }
        let fail = env("MOCK_FAIL_FLAG");
        if !fail.is_empty() && Path::new(&fail).exists() {
            println!(r#"MSG:2003,0,3,"Error 'Scsi error - MEDIUM ERROR' occurred while reading","x""#);
            println!(r#"MSG:5003,0,2,"Failed to save title 0 to file","x""#);
            std::process::exit(0); // like MakeMKV: exit code 0, but no file
        }
        std::fs::write(Path::new(out_dir).join(format!("title_t{real:02}.mkv")), format!("MOCKMKV d{drive} title {real}\n"))
            .expect("write mkv");
        println!(r#"MSG:5036,260,1,"Copy complete. 1 titles saved.","Copy complete. %1 titles saved.","1""#);
        return;
    }
    std::process::exit(1);
}
