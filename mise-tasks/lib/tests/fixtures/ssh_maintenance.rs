//! Compiled only in isolated source copies by server:ssh:trust:test. The supervisor is the
//! OS boundary; artifact authentication, maintenance, confirmations and recovery are real.
use super::*;
use crate::{
    host::Paths,
    organization::Organization,
    service::{State, Supervisor},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

const IDENTITY: &str = "spaceterm-development";
const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const VERSION: &str = "dev.aaaaaaaaaaaa";

struct Manager(PathBuf);
impl Supervisor for Manager {
    fn startup(&self) -> ServiceStartup {
        ServiceStartup::AtLogin
    }
    fn check(&self, runtime: &Path, _: bool) -> Result<(), ServerError> {
        if std::fs::read_to_string(self.0.join("supervisor")).unwrap() == runtime.to_str().unwrap()
        {
            Ok(())
        } else {
            Err(ServerError::CleanupUnresolved)
        }
    }
    fn install(&self, _: &Path) -> Result<(), ServerError> {
        panic!("no installation authorized")
    }
    fn prepare_start(&self) -> Result<(), ServerError> {
        panic!("Stop must be preserved")
    }
    fn start(&self) -> Result<(), ServerError> {
        panic!("no Start authorized")
    }
    fn disable(&self) -> Result<(), ServerError> {
        panic!("stopped runtime needs no Stop")
    }
    fn uninstall(&self, _: &Path) -> Result<(), ServerError> {
        panic!("no uninstall authorized")
    }
    fn running(&self) -> Result<bool, ServerError> {
        Ok(false)
    }
    fn replace(&self, previous: &Path, candidate: &Path) -> Result<(), ServerError> {
        self.check(previous, false)?;
        if self.0.join("interrupt-replace").exists() {
            std::fs::remove_file(self.0.join("interrupt-replace")).unwrap();
            return Err(ServerError::ServiceUnavailable);
        }
        std::fs::write(self.0.join("supervisor"), candidate.to_str().unwrap()).unwrap();
        Ok(())
    }
}

pub fn run() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let target = if cfg!(target_os = "macos") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    if args == ["--build-info"] {
        println!(
            "{}",
            serde_json::json!({"schema":1,"identity":IDENTITY,"target":target,"commit":COMMIT,"version":VERSION,"protocol_major":1,"capabilities":["workspace_policy"]})
        );
        return;
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let root = home.join(".local/share").join(IDENTITY).join("server");
    let paths = Paths {
        identity: IDENTITY.into(),
        data: root.clone(),
        runtime: root.join("runtime"),
    };
    let service = Service::new(paths, IDENTITY.into(), Arc::new(Manager(root.clone())));
    if let [command, digest] = args.as_slice()
        && command == "fixture-seed"
    {
        let data = Directory::open(&root, true).unwrap();
        let mut record = data.read_or_create().unwrap();
        let mut organization = Organization::first(home).unwrap();
        organization.name = "Retained SSH work".into();
        record.organization = Some(organization);
        data.write(&record).unwrap();
        data.write_json(
            c"service.json",
            &State {
                installation: digest.clone(),
                stopped: true,
                installed: true,
                removing: false,
                run: None,
                activation: None,
            },
        )
        .unwrap();
        std::fs::write(
            root.join("supervisor"),
            root.join("installations").join(digest).to_str().unwrap(),
        )
        .unwrap();
        return;
    }
    assert_eq!(args.get(1).map(String::as_str), Some(IDENTITY));
    if args[0] == "ssh-discover" {
        let data = Directory::open(&root, false).unwrap();
        let status = service.status().unwrap();
        println!(
            "{}",
            serde_json::json!({"schema":1,"account":unsafe { libc::geteuid() },"build":IDENTITY,"server":data.read().unwrap().server,"installed":status.installed,"enabled":status.enabled,"running":status.running,"stopped":status.stopped,"protocol_majors":[1],"capabilities":["workspace_policy"],"target":target,"version":VERSION,"commit":COMMIT})
        );
        return;
    }
    if args[0] == "ssh-stage" {
        assert_eq!(args.len(), 5);
        let retained = service
            .provision_candidate(
                Path::new(&args[2]),
                Path::new(&args[3]),
                &args[4],
                1,
                &["workspace_policy"],
            )
            .unwrap();
        println!(
            "{}",
            serde_json::to_string(&Reply::Staged {
                candidate: RemoteBuild::describe(&retained).unwrap()
            })
            .unwrap()
        );
        return;
    }
    assert_eq!(args[0], "ssh-maintain");
    Maintenance::new(service)
        .exchange(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
        .unwrap();
}
