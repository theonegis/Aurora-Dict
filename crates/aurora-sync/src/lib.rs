//! Local-only HTTPS sync host. Pairing is explicit; discovery never grants access.
use aurora_core::{sync, vocabulary};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
const SERVICE: &str = "_aurora-sync._tcp.local.";
fn id() -> String {
    Uuid::new_v4().simple().to_string()
}
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub book_id: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub token_hash: String,
}
#[derive(Serialize, Deserialize)]
struct Config {
    server_id: String,
    cert: String,
    key: String,
    fingerprint: String,
    enabled: bool,
    peers: Vec<Peer>,
}
fn save(path: &Path, c: &Config) -> Result<(), String> {
    fs::create_dir_all(path.parent().ok_or("无配置目录")?).map_err(|e| e.to_string())?;
    let mut tmp =
        tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(|e| e.to_string())?;
    tmp.write_all(&serde_json::to_vec(c).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    tmp.as_file().sync_all().map_err(|e| e.to_string())?;
    tmp.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
fn config(path: &Path) -> Result<Config, String> {
    if path.exists() {
        return serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string());
    }
    let server_id = id();
    let cert = rcgen::generate_simple_self_signed(vec![
        format!("aurora-{server_id}.local"),
        "localhost".into(),
    ])
    .map_err(|e| e.to_string())?;
    let c = Config {
        server_id,
        cert: cert.cert.pem(),
        key: cert.key_pair.serialize_pem(),
        fingerprint: format!("{:x}", Sha256::digest(cert.cert.der())),
        enabled: false,
        peers: vec![],
    };
    save(path, &c)?;
    Ok(c)
}
pub fn enabled(data: &Path) -> bool {
    config(&data.join("lan-sync.json"))
        .map(|v| v.enabled)
        .unwrap_or(false)
}
pub fn disable(data: &Path) -> Result<(), String> {
    let path = data.join("lan-sync.json");
    let mut c = config(&path)?;
    c.enabled = false;
    save(&path, &c)
}
struct Invite {
    secret: String,
    expires: u64,
    claimed: Option<(String, String)>,
}
struct Shared {
    config: Mutex<Config>,
    config_path: PathBuf,
    book: PathBuf,
    book_id: String,
    invite: Mutex<Option<Invite>>,
    on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pairing {
    pub protocol: u32,
    pub server_id: String,
    pub book_id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub secret: String,
    pub expires: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub enabled: bool,
    pub server_id: String,
    pub book_id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub peers: Vec<Peer>,
    pub discovery_error: Option<String>,
}
pub struct Host {
    shared: Arc<Shared>,
    handle: axum_server::Handle,
    mdns: Option<mdns_sd::ServiceDaemon>,
    name: String,
    host: String,
    port: u16,
    discovery_error: Option<String>,
}
impl Host {
    pub async fn start(data: &Path, book: PathBuf) -> Result<Self, String> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let path = data.join("lan-sync.json");
        let mut c = config(&path)?;
        let book_id = sync::book_id(&vocabulary::connect(&book)?)?;
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            c.cert.as_bytes().to_vec(),
            c.key.as_bytes().to_vec(),
        )
        .await
        .map_err(|e| e.to_string())?;
        let listener = TcpListener::bind(("0.0.0.0", 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let host = format!("aurora-{}.local.", c.server_id);
        let name = format!("Aurora-{}", &c.server_id[..8]);
        let (mdns, discovery_error) = match mdns_sd::ServiceDaemon::new() {
            Ok(d) => {
                let info = mdns_sd::ServiceInfo::new(
                    SERVICE,
                    &name,
                    &host,
                    "",
                    port,
                    [("serverId", c.server_id.as_str()), ("protocol", "1")].as_slice(),
                )
                .map_err(|e| e.to_string())?
                .enable_addr_auto();
                match d.register(info) {
                    Ok(()) => (Some(d), None),
                    Err(e) => {
                        let _ = d.shutdown();
                        (None, Some(e.to_string()))
                    }
                }
            }
            Err(e) => (None, Some(e.to_string())),
        };
        c.enabled = true;
        save(&path, &c)?;
        let shared = Arc::new(Shared {
            config: Mutex::new(c),
            config_path: path,
            book,
            book_id,
            invite: Mutex::new(None),
            on_change: Mutex::new(None),
        });
        let app = Router::new()
            .route("/v1/pair", post(pair))
            .route("/v1/exchange", post(exchange))
            .route("/v1/resolve", post(resolve))
            .route("/v1/health", get(health))
            .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
            .with_state(shared.clone());
        let handle = axum_server::Handle::new();
        let server = handle.clone();
        tokio::spawn(async move {
            let _ = axum_server::from_tcp_rustls(listener, tls)
                .handle(server)
                .serve(app.into_make_service())
                .await;
        });
        Ok(Self {
            shared,
            handle,
            mdns,
            name,
            host,
            port,
            discovery_error,
        })
    }
    pub fn set_on_change(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self.shared.on_change.lock().unwrap() = Some(f);
    }
    pub fn status(&self) -> Status {
        let c = self.shared.config.lock().unwrap();
        Status {
            enabled: true,
            server_id: c.server_id.clone(),
            book_id: self.shared.book_id.clone(),
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port,
            peers: c
                .peers
                .iter()
                .map(|p| Peer {
                    token_hash: String::new(),
                    ..p.clone()
                })
                .collect(),
            discovery_error: self.discovery_error.clone(),
        }
    }
    pub fn pairing(&self, host: Option<String>) -> Result<(Pairing, String), String> {
        // Match the pairing handler lock order: invitation before config.
        let mut invite = self.shared.invite.lock().map_err(|e| e.to_string())?;
        let c = self.shared.config.lock().map_err(|e| e.to_string())?;
        let secret = format!("{}{}", id(), id());
        let expires = now() + 300;
        *invite = Some(Invite {
            secret: secret.clone(),
            expires,
            claimed: None,
        });
        let p = Pairing {
            protocol: 1,
            server_id: c.server_id.clone(),
            book_id: self.shared.book_id.clone(),
            name: self.name.clone(),
            host: host
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| self.host.clone()),
            port: self.port,
            fingerprint: c.fingerprint.clone(),
            secret,
            expires,
        };
        let text = serde_json::to_string(&p).map_err(|e| e.to_string())?;
        let qr = qrcode::QrCode::new(text.as_bytes())
            .map_err(|e| e.to_string())?
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(240, 240)
            .build();
        Ok((p, qr))
    }
    pub fn revoke(&self, device: &str) -> Result<(), String> {
        let mut c = self.shared.config.lock().map_err(|e| e.to_string())?;
        c.peers.retain(|p| p.id != device);
        save(&self.shared.config_path, &c)
    }
    pub fn stop(&self) {
        self.handle.shutdown();
        if let Some(d) = &self.mdns {
            let _ = d.shutdown();
        }
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.stop();
    }
}
type Error = (StatusCode, Json<serde_json::Value>);
fn err(code: StatusCode, s: impl ToString) -> Error {
    (code, Json(serde_json::json!({"error":s.to_string()})))
}
fn authenticate(s: &Shared, h: &HeaderMap) -> Result<String, Error> {
    let token = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "请重新配对电脑。"))?;
    let c = s
        .config
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "设备状态不可用"))?;
    c.peers
        .iter()
        .find(|p| p.token_hash == hash(token) && p.book_id == s.book_id)
        .map(|p| p.id.clone())
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "配对已撤销，请重新配对。"))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    protocol: u32,
    secret: String,
    device_id: String,
    name: String,
    token: String,
    book_id: String,
}
async fn pair(
    State(s): State<Arc<Shared>>,
    Json(r): Json<PairRequest>,
) -> Result<Json<serde_json::Value>, Error> {
    if r.protocol != 1
        || r.book_id != s.book_id
        || r.device_id.len() != 32
        || r.token.len() != 64
        || r.name.len() > 120
    {
        return Err(err(StatusCode::BAD_REQUEST, "无效的配对请求"));
    }
    // The persisted book may have been replaced while the server was running.
    let current =
        sync::book_id(&vocabulary::connect(&s.book).map_err(|e| err(StatusCode::CONFLICT, e))?)
            .map_err(|e| err(StatusCode::CONFLICT, e))?;
    if current != s.book_id {
        return Err(err(StatusCode::CONFLICT, "生词本已更换，请重新开启同步"));
    }
    let mut invite = s
        .invite
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "配对状态不可用"))?;
    let invite = invite
        .as_mut()
        .filter(|i| i.expires >= now() && i.secret == r.secret)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "配对码已失效，请在电脑上刷新"))?;
    let token_hash = hash(&r.token);
    if let Some((device, token)) = &invite.claimed {
        if device != &r.device_id || token != &token_hash {
            return Err(err(StatusCode::UNAUTHORIZED, "配对码已使用"));
        }
    }
    let mut c = s
        .config
        .lock()
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "设备状态不可用"))?;
    if c.peers
        .iter()
        .any(|p| p.id == r.device_id && p.token_hash != token_hash)
    {
        return Err(err(StatusCode::CONFLICT, "设备编号已使用，请重新生成配对"));
    }
    c.peers.retain(|p| p.id != r.device_id);
    c.peers.push(Peer {
        id: r.device_id.clone(),
        name: r.name,
        book_id: s.book_id.clone(),
        token_hash: token_hash.clone(),
    });
    save(&s.config_path, &c).map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    invite.claimed = Some((r.device_id, token_hash));
    Ok(Json(
        serde_json::json!({"protocol":1,"serverId":c.server_id,"bookId":s.book_id}),
    ))
}
async fn health(
    State(s): State<Arc<Shared>>,
    h: HeaderMap,
) -> Result<Json<serde_json::Value>, Error> {
    authenticate(&s, &h)?;
    Ok(Json(serde_json::json!({"protocol":1,"bookId":s.book_id})))
}
async fn exchange(
    State(s): State<Arc<Shared>>,
    h: HeaderMap,
    Json(r): Json<sync::Exchange>,
) -> Result<Json<sync::Reply>, Error> {
    let device = authenticate(&s, &h)?;
    tokio::task::spawn_blocking(move || {
        let mut c = vocabulary::connect(&s.book)?;
        let reply = sync::exchange(&mut c, &device, &r)?;
        if !r.changes.is_empty() {
            if let Some(f) = s.on_change.lock().unwrap().as_ref() {
                f();
            }
        }
        Ok::<_, String>(reply)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .map(Json)
    .map_err(|e| err(StatusCode::CONFLICT, e))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Resolution {
    book_id: String,
    id: String,
    use_candidate: bool,
    expected_version: String,
}
async fn resolve(
    State(s): State<Arc<Shared>>,
    h: HeaderMap,
    Json(r): Json<Resolution>,
) -> Result<Json<serde_json::Value>, Error> {
    authenticate(&s, &h)?;
    tokio::task::spawn_blocking(move || {
        let mut c = vocabulary::connect(&s.book)?;
        if sync::book_id(&c)? != r.book_id {
            return Err("生词本已更换，请重新配对。".into());
        }
        sync::resolve(&mut c, &r.id, r.use_candidate, &r.expected_version)?;
        if let Some(f) = s.on_change.lock().unwrap().as_ref() {
            f();
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .map_err(|e| err(StatusCode::CONFLICT, e))?;
    Ok(Json(serde_json::json!({"ok":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn refreshing_qr_never_holds_config_while_waiting_for_invitation() {
        let d = tempfile::tempdir().unwrap();
        let host = Arc::new(
            Host::start(d.path(), d.path().join("book.db"))
                .await
                .unwrap(),
        );
        let invitation = host.shared.invite.lock().unwrap();
        let (started, ready) = std::sync::mpsc::channel();
        let h = host.clone();
        let worker = std::thread::spawn(move || {
            started.send(()).unwrap();
            h.pairing(None)
        });
        ready.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let available = host.shared.config.try_lock().is_ok();
        drop(invitation);
        assert!(worker.join().unwrap().is_ok());
        assert!(
            available,
            "QR refresh inverted invitation/config lock order"
        );
    }

    #[tokio::test]
    async fn pairing_is_one_device_idempotent_and_revocable() {
        let d = tempfile::tempdir().unwrap();
        let book = d.path().join("test.sqlite3");
        let host = Host::start(d.path(), book.clone()).await.unwrap();
        let (invite, _) = host.pairing(Some("127.0.0.1".into())).unwrap();
        let pem = host.shared.config.lock().unwrap().cert.clone();
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes()).unwrap())
            .build()
            .unwrap();
        // Certificate SAN includes localhost, with the same persisted certificate after restart.
        let url = format!("https://localhost:{}", invite.port);
        let token = format!("{}{}", id(), id());
        let device = id();
        let req = serde_json::json!({"protocol":1,"bookId":invite.book_id,"secret":invite.secret,"deviceId":device,"token":token,"name":"Test phone"});
        let body =
            serde_json::json!({"protocol":1,"bookId":invite.book_id,"cursor":0,"changes":[]});
        assert_eq!(
            client
                .post(format!("{url}/v1/exchange"))
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        for _ in 0..2 {
            assert_eq!(
                client
                    .post(format!("{url}/v1/pair"))
                    .json(&req)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                200
            );
        }
        let mut other = req.clone();
        other["deviceId"] = id().into();
        assert_eq!(
            client
                .post(format!("{url}/v1/pair"))
                .json(&other)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client
                .post(format!("{url}/v1/exchange"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        drop(host);
        let host = Host::start(d.path(), book).await.unwrap();
        let url = format!("https://localhost:{}", host.status().port);
        assert_eq!(
            client
                .post(format!("{url}/v1/exchange"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        host.revoke(&device).unwrap();
        assert_eq!(
            client
                .post(format!("{url}/v1/exchange"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
}
