//! Run the foreground owner composition contract without a desktop or GPU.
//! The desktop target runs the same suite against the same native provisioner.

#[expect(
    dead_code,
    reason = "the headless suite uses the injected credential boundary; desktop calls the native credential entry"
)]
#[path = "../../../src/platform/local_server_client/enrollment.rs"]
mod enrollment;

mod platform {
    pub mod local_server_client {
        pub mod tests {
            use ring::rand::{SecureRandom, SystemRandom};
            use std::{os::unix::fs::DirBuilderExt as _, path::PathBuf};

            pub(crate) struct Root(pub(crate) PathBuf);
            impl Root {
                pub(crate) fn new() -> Self {
                    let mut suffix = [0; 16];
                    SystemRandom::new().fill(&mut suffix).unwrap();
                    #[cfg(target_os = "macos")]
                    let parent = PathBuf::from("/private/tmp");
                    #[cfg(not(target_os = "macos"))]
                    let parent = PathBuf::from("/var/tmp");
                    let path = parent.join(format!("st-owner-{:x}", u128::from_be_bytes(suffix)));
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&path)
                        .unwrap();
                    Self(path)
                }
            }
            impl Drop for Root {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
        }
    }
}
