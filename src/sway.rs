//! Minimal sway IPC client: `get_tree`, `run_command`, subscribe.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use anyhow::{Context, Result, bail};

use crate::model::Tree;

const MAGIC: &[u8; 6] = b"i3-ipc";
const RUN_COMMAND: u32 = 0;
const SUBSCRIBE: u32 = 2;
const GET_TREE: u32 = 4;
const GET_VERSION: u32 = 7;
/// Far above any real reply; guards against allocating garbage lengths.
const MAX_MESSAGE: usize = 64 << 20;

pub struct Ipc(UnixStream);

impl Ipc {
    pub fn connect() -> Result<Self> {
        let path = std::env::var("SWAYSOCK").context("SWAYSOCK is not set")?;
        let stream = UnixStream::connect(&path).with_context(|| format!("connecting to {path}"))?;
        Ok(Ipc(stream))
    }

    fn request(&mut self, ty: u32, payload: &str) -> Result<Vec<u8>> {
        let mut msg = Vec::with_capacity(14 + payload.len());
        msg.extend_from_slice(MAGIC);
        msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        msg.extend_from_slice(&ty.to_ne_bytes());
        msg.extend_from_slice(payload.as_bytes());
        self.0.write_all(&msg)?;
        let (reply_ty, body) = self.read_message()?;
        if reply_ty != ty {
            bail!("unexpected IPC reply type {reply_ty:#x} to {ty}");
        }
        Ok(body)
    }

    fn read_message(&mut self) -> Result<(u32, Vec<u8>)> {
        let mut header = [0u8; 14];
        self.0.read_exact(&mut header)?;
        let [m0, m1, m2, m3, m4, m5, l0, l1, l2, l3, t0, t1, t2, t3] = header;
        if [m0, m1, m2, m3, m4, m5] != *MAGIC {
            bail!("bad IPC magic");
        }
        let len = u32::from_ne_bytes([l0, l1, l2, l3]) as usize;
        let ty = u32::from_ne_bytes([t0, t1, t2, t3]);
        if len > MAX_MESSAGE {
            bail!("IPC message of {len} bytes is too large");
        }
        let mut body = vec![0; len];
        self.0.read_exact(&mut body)?;
        Ok((ty, body))
    }

    pub fn get_tree(&mut self) -> Result<Tree> {
        Tree::from_json(&self.request(GET_TREE, "")?)
    }

    /// Path of the config file sway loaded.
    pub fn config_path(&mut self) -> Result<std::path::PathBuf> {
        let reply: serde_json::Value = serde_json::from_slice(&self.request(GET_VERSION, "")?)?;
        let path = reply["loaded_config_file_name"].as_str().context("no config path")?;
        Ok(path.into())
    }

    pub fn command(&mut self, cmd: &str) -> Result<()> {
        let reply: serde_json::Value = serde_json::from_slice(&self.request(RUN_COMMAND, cmd)?)?;
        for r in reply.as_array().into_iter().flatten() {
            if r["success"] != true {
                bail!("sway command {cmd:?} failed: {}", r["error"]);
            }
        }
        Ok(())
    }

    /// Subscribes to `events` and calls `on_event` for each one until the connection fails.
    pub fn subscribe(mut self, events: &[&str], mut on_event: impl FnMut()) -> Result<()> {
        let reply: serde_json::Value =
            serde_json::from_slice(&self.request(SUBSCRIBE, &serde_json::to_string(events)?)?)?;
        if reply["success"] != true {
            bail!("subscribe failed: {reply}");
        }
        loop {
            self.read_message()?;
            on_event();
        }
    }
}
