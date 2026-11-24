//! Starts the real server against the mock makemkvcon (two drives) and
//! drives it over HTTP the way the web UI does.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server {
    child: Child,
    port: u16,
    tmp: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

type HttpResult = Result<Value, (u16, String)>;

impl Server {
    fn start() -> Server {
        let tmp = std::env::temp_dir().join(format!("sr-it-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_mock-makemkvcon"), bin.join("makemkvcon")).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let out = tmp.join("output");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kare_kano_vol5_info.txt");
        let child = Command::new(env!("CARGO_BIN_EXE_series-ripper"))
            .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()))
            .env("HOME", tmp.join("home"))
            .env("DVDRIPPER_DB", tmp.join("config/test.db"))
            .env("DVDRIPPER_TEMP", out.join(".series-ripper-tmp"))
            .env("DVDRIPPER_PORT", port.to_string())
            .env("DVDRIPPER_NO_IOCTL", "1")
            .env("MAKEMKV_KEY", "T-testkey-for-the-integration-test-only")
            .env("PUID", unsafe { libc::getuid() }.to_string())
            .env("PGID", unsafe { libc::getgid() }.to_string())
            .env("MOCK_FIXTURE", fixture)
            .env("MOCK_DRIVES", "2")
            .env("MOCK_DISC_FLAG", tmp.join("disc"))
            .env("MOCK_FAIL_FLAG", tmp.join("fail"))
            .env("MOCK_SCAN_DELAY", "0.5")
            .env("MOCK_RIP_STEP_MS", "100")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let s = Server { child, port, tmp };
        s.wait_for(|| s.call("GET", "/api/health", None).ok().map(|_| ()), "server start");
        s
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> HttpResult {
        let url = format!("http://127.0.0.1:{}{}", self.port, path);
        let req = ureq::request(method, &url).timeout(Duration::from_secs(30));
        let res = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        match res {
            Ok(r) => {
                let text = r.into_string().unwrap_or_default();
                Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
            }
            Err(ureq::Error::Status(code, r)) => Err((code, r.into_string().unwrap_or_default())),
            Err(e) => Err((0, e.to_string())),
        }
    }

    fn get(&self, path: &str) -> Value {
        self.call("GET", path, None).unwrap_or_else(|e| panic!("GET {path} -> {e:?}"))
    }

    fn post(&self, path: &str, body: Value) -> Value {
        self.call("POST", path, Some(body)).unwrap_or_else(|e| panic!("POST {path} -> {e:?}"))
    }

    fn logs(&self) -> Vec<String> {
        self.get("/api/logs?limit=2000")["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["message"].as_str().unwrap().to_string())
            .collect()
    }

    fn log_has(&self, text: &str) -> bool {
        self.logs().iter().any(|m| m.contains(text))
    }

    fn wait_for<T>(&self, mut check: impl FnMut() -> Option<T>, what: &str) -> T {
        let end = Instant::now() + Duration::from_secs(60);
        while Instant::now() < end {
            if let Some(v) = check() {
                return v;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let logs = self.call("GET", "/api/logs?limit=40", None).map(|v| v.to_string()).unwrap_or_default();
        panic!("timed out waiting for {what}; recent log: {logs}");
    }

    fn drive(&self, id: i64) -> Value {
        self.get("/api/drives")["drives"].as_array().unwrap().iter().find(|d| d["id"] == id).unwrap().clone()
    }

    fn wait_idle(&self, id: i64) -> Value {
        self.wait_for(
            || {
                let d = self.drive(id);
                (d["activity"] == "idle").then_some(d)
            },
            "drive idle",
        )
    }

    fn disc(&self, drive: usize, inserted: bool) {
        let flag = self.tmp.join(format!("disc.{drive}"));
        if inserted {
            std::fs::write(flag, b"").unwrap();
        } else {
            let _ = std::fs::remove_file(flag);
        }
    }

    fn scan(&self, drive: i64, show: i64) -> Value {
        let r = self.post(&format!("/api/drives/{drive}/scan"), json!({ "show_id": show }));
        assert_eq!(r["started"], true);
        let scan = self.wait_for(
            || {
                let s = self.get(&format!("/api/drives/{drive}/scan"));
                (s["running"] == false).then_some(s)
            },
            "scan",
        );
        assert!(scan["error"].is_null(), "scan failed: {}", scan["error"]);
        scan["result"].clone()
    }

    fn show(&self, id: i64) -> Value {
        self.get("/api/shows")["shows"].as_array().unwrap().iter().find(|s| s["id"] == id).unwrap().clone()
    }

    fn out(&self) -> PathBuf {
        self.tmp.join("output")
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.out().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }
}

fn episodes(rows: &Value) -> Vec<Option<i64>> {
    rows.as_array().unwrap().iter().map(|r| r["episode"].as_i64()).collect()
}

#[test]
fn full_flow() {
    let s = Server::start();

    // --- basics: version, license key, two drives
    assert_eq!(s.get("/api/status")["makemkv_version"], "MakeMKV v2.0.0 linux(x64-release)");
    s.wait_for(|| (s.get("/api/status")["license"]["mode"] == "custom").then_some(()), "license");
    let conf = std::fs::read_to_string(s.tmp.join("home/.MakeMKV/settings.conf")).unwrap();
    assert!(conf.contains("app_Key = \"T-testkey-for-the-integration-test-only\""));
    let drives = s.get("/api/drives")["drives"].as_array().unwrap().clone();
    assert_eq!(drives.len(), 2);
    assert_eq!(drives[0]["device"], "/dev/sr0");
    assert_eq!(drives[1]["device"], "/dev/sr1");
    let (d0, d1) = (drives[0]["id"].as_i64().unwrap(), drives[1]["id"].as_i64().unwrap());

    let kk = s.post("/api/shows", json!({"name": "Kare Kano", "season": 1, "next_episode": 14}))["id"].as_i64().unwrap();
    let other = s.post("/api/shows", json!({"name": "Other Show", "season": 2}))["id"].as_i64().unwrap();
    assert!(s.call("POST", "/api/shows", Some(json!({"name": ""}))).is_err());
    let out = s.out();
    s.post(
        "/api/config",
        json!({"output_dir": out.display().to_string(), "poll_interval_sec": 2, "detect_play_all": false}),
    );

    // --- Automatic Mode with Play All detection off: the 70-minute title
    // would be ripped as an episode, so the disc is parked for review.
    s.post(&format!("/api/drives/{d0}/settings"), json!({"show_id": kk, "automatic": true}));
    s.disc(0, true);
    let items = s.wait_for(
        || {
            let v = s.get("/api/review")["items"].clone();
            (!v.as_array().unwrap().is_empty()).then_some(v)
        },
        "review item",
    );
    let item = &items[0];
    assert!(item["reason"].as_str().unwrap().contains("long-title"), "{}", item["reason"]);
    assert_eq!(item["disc_label"], "KARE_KANO_VOL5");
    let names: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(names, vec![".series-ripper-tmp"]);
    let mut rows = item["rows"].clone();
    assert_eq!(&episodes(&rows)[..4], &[Some(14), Some(15), Some(16), Some(17)]);
    // What the UI does when the Play All row is unticked: renumber the rest.
    rows[0]["include"] = json!(false);
    rows[0]["episode"] = Value::Null;
    for (i, ep) in [(1, 14), (2, 15), (3, 16)] {
        rows[i]["episode"] = json!(ep);
    }
    s.post(&format!("/api/review/{}/resolve", item["id"]), json!({"rows": rows}));
    s.wait_for(|| s.log_has("Disc complete.").then_some(()), "review rip");
    let d = s.wait_idle(d0);
    assert!(d["last_message"].as_str().unwrap().starts_with("Disc complete: 3 title(s) saved (S01E14-S01E16)"));
    assert_eq!(s.read("Kare Kano/Season 1/Kare Kano - S01E14.mkv"), "MOCKMKV d0 title 1\n");
    assert_eq!(s.read("Kare Kano/Season 1/Kare Kano - S01E15.mkv"), "MOCKMKV d0 title 2\n");
    assert_eq!(s.read("Kare Kano/Season 1/Kare Kano - S01E16.mkv"), "MOCKMKV d0 title 3\n");
    assert_eq!(s.show(kk)["next_episode"], 17);
    let mut tmp_names: Vec<_> = std::fs::read_dir(out.join(".series-ripper-tmp"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    tmp_names.sort();
    assert_eq!(tmp_names, vec![".ignore", ".plexignore"]);
    assert!(s.get("/api/review")["items"].as_array().unwrap().is_empty());

    // --- Same disc again (e.g. restart with it still in the tray): skipped.
    s.disc(0, false);
    s.wait_for(|| (s.drive(d0)["has_disc"] == false).then_some(()), "disc removed");
    s.disc(0, true);
    s.wait_for(|| s.log_has("was already ripped completely").then_some(()), "duplicate skip");
    s.wait_idle(d0);

    // --- Two drives, two shows, ripping at the same time.
    s.post(&format!("/api/drives/{d0}/settings"), json!({"automatic": false}));
    s.post("/api/config", json!({"detect_play_all": true}));
    s.disc(1, true);
    s.post(&format!("/api/drives/{d0}/scan"), json!({"show_id": kk}));
    let r1 = s.scan(d1, other);
    let r0 = s.wait_for(
        || {
            let v = s.get(&format!("/api/drives/{d0}/scan"));
            (v["running"] == false).then_some(v["result"].clone())
        },
        "scan d0",
    );
    assert_eq!(episodes(&r0["rows"]), vec![None, Some(17), Some(18), Some(19), None, None]);
    assert_eq!(episodes(&r1["rows"]), vec![None, Some(1), Some(2), Some(3), None, None]);
    assert_eq!(r1["disc_name"], "KARE_KANO_VOL6");
    s.post(&format!("/api/drives/{d0}/rip"), json!({}));
    s.post(&format!("/api/drives/{d1}/rip"), json!({}));
    let mut both = false;
    s.wait_for(
        || {
            let st = s.get("/api/status");
            let busy: Vec<_> = st["drives"].as_array().unwrap().iter().filter(|d| d["activity"] == "ripping").collect();
            if busy.len() == 2 {
                both = true;
            }
            busy.is_empty().then_some(())
        },
        "parallel rips",
    );
    assert!(both, "the two drives never ripped at the same time");
    assert_eq!(s.read("Kare Kano/Season 1/Kare Kano - S01E17.mkv"), "MOCKMKV d0 title 1\n");
    assert_eq!(s.read("Kare Kano/Season 1/Kare Kano - S01E19.mkv"), "MOCKMKV d0 title 3\n");
    assert_eq!(s.read("Other Show/Season 2/Other Show - S02E01.mkv"), "MOCKMKV d1 title 1\n");
    assert_eq!(s.read("Other Show/Season 2/Other Show - S02E03.mkv"), "MOCKMKV d1 title 3\n");
    assert_eq!(s.show(kk)["next_episode"], 20);
    assert_eq!(s.show(other)["next_episode"], 4);

    // --- Same show in both drives with the same numbers: the second is refused.
    s.post(&format!("/api/drives/{d0}/scan"), json!({"show_id": kk}));
    let r1 = s.scan(d1, kk);
    s.wait_idle(d0);
    assert_eq!(episodes(&r1["rows"])[1], Some(20));
    s.post(&format!("/api/drives/{d0}/rip"), json!({}));
    let err = s.call("POST", &format!("/api/drives/{d1}/rip"), Some(json!({}))).unwrap_err();
    assert_eq!(err.0, 409);
    assert!(err.1.contains("already being ripped"), "{}", err.1);
    s.wait_idle(d0);
    assert_eq!(s.show(kk)["next_episode"], 23);

    // --- A failing rip goes to the Review Queue for a retry.
    let r1 = s.scan(d1, kk);
    assert_eq!(episodes(&r1["rows"])[1..4], [Some(23), Some(24), Some(25)]);
    std::fs::write(s.tmp.join("fail"), b"").unwrap();
    s.post(&format!("/api/drives/{d1}/rip"), json!({}));
    let d = s.wait_idle(d1);
    assert!(d["last_message"].as_str().unwrap().contains("3 failed"), "{}", d["last_message"]);
    let items = s.get("/api/review")["items"].clone();
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert!(items[0]["reason"].as_str().unwrap().contains("failed"));
    assert_eq!(episodes(&items[0]["rows"]), vec![Some(23), Some(24), Some(25)]);
    assert_eq!(s.show(kk)["next_episode"], 26, "failed numbers stay reserved for the retry");
    std::fs::remove_file(s.tmp.join("fail")).unwrap();
    // The review item remembers drive and show; the job starts before the reply.
    s.post(&format!("/api/review/{}/resolve", items[0]["id"]), json!({}));
    s.wait_idle(d1);
    assert_eq!(s.read("Kare Kano/Season 1/Kare Kano - S01E25.mkv"), "MOCKMKV d1 title 3\n");
    assert!(s.get("/api/review")["items"].as_array().unwrap().is_empty());

    // --- Cancel: the numbers that weren't ripped go back to the show.
    s.scan(d0, kk);
    s.post(&format!("/api/drives/{d0}/rip"), json!({}));
    assert_eq!(s.show(kk)["next_episode"], 29);
    s.post(&format!("/api/drives/{d0}/cancel"), json!({}));
    let d = s.wait_idle(d0);
    assert!(d["last_message"].as_str().unwrap().starts_with("Cancelled"), "{}", d["last_message"]);
    let saved = (26..29)
        .filter(|e| out.join(format!("Kare Kano/Season 1/Kare Kano - S01E{e}.mkv")).exists())
        .count() as i64;
    assert_eq!(s.show(kk)["next_episode"], 26 + saved);

    // --- A Special with an episode title.
    let r0 = s.scan(d0, kk);
    let mut rows = r0["rows"].clone();
    for r in rows.as_array_mut().unwrap() {
        r["include"] = json!(r["title_index"] == 4);
    }
    rows[4]["special"] = json!(true);
    rows[4]["episode"] = json!(1);
    rows[4]["episode_title"] = json!("Bonus: Making of");
    s.post(&format!("/api/drives/{d0}/rip"), json!({"rows": rows}));
    s.wait_idle(d0);
    assert_eq!(s.read("Kare Kano/Season 0/Kare Kano - S00E01 - Bonus Making of.mkv"), "MOCKMKV d0 title 4\n");
    assert_eq!(s.show(kk)["next_special"], 2);

    // --- History: every title, with its result.
    let hist = s.get("/api/history?limit=500")["entries"].as_array().unwrap().clone();
    let ok = hist.iter().filter(|h| h["status"] == "ok").count() as i64;
    let failed = hist.iter().filter(|h| h["status"] == "failed").count();
    assert_eq!(failed, 3);
    assert_eq!(ok, 3 + 6 + 3 + 3 + saved + 1);
    let only_other = s.get(&format!("/api/history?show_id={other}"))["entries"].as_array().unwrap().len();
    assert_eq!(only_other, 3);

    // --- Drive settings persist, the web UI is served, bad input is refused.
    s.post(&format!("/api/drives/{d1}/settings"), json!({"nickname": "Pioneer"}));
    assert_eq!(s.drive(d1)["label"], "Pioneer");
    let html = s.get("/");
    assert!(html.as_str().unwrap().contains("Series Ripper"));
    let err = s.call("POST", &format!("/api/drives/{d0}/rip"), Some(json!({"rows": [{"title_index": 1, "include": true}]})));
    assert_eq!(err.unwrap_err().0, 400);
}
