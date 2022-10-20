use serde::{Deserialize, Serialize};

use poem::{
	get, handler,
	http::StatusCode,
	listener::TcpListener,
	web::{Json, Path},
	IntoResponse, Route, Server,
};

use figment::{providers::Env, Figment};

use prometheus_http_query::Client;

#[derive(Deserialize)]
struct Config {
	#[serde(default = "prometheus_url_default")]
	prometheus_url: String,
	#[serde(default = "bind_address")]
	bind_address: String,
	#[serde(default = "domain_allowlist_default")]
	domain_allowlist: Vec<String>,
}

#[derive(Serialize)]
struct UptimeResponse {
	domain: String,
	t0: u64,
	uptime_history: Vec<Option<f64>>,
}

#[derive(Serialize)]
struct ErrorResponse {
	message: String,
}

#[derive(Serialize)]
#[serde(tag = "status")]
enum Response {
	#[serde(rename = "success")]
	Success(UptimeResponse),
	#[serde(rename = "error")]
	Error(ErrorResponse),
}

lazy_static::lazy_static! {
	static ref CONFIG: Config = {
		let config: Config = Figment::new()
			.merge(Env::prefixed("UPTIMEPROXY_"))
			.extract()
			.expect("invalid configuration");
		config
	};
}

fn prometheus_url_default() -> String {
	"http://localhost:9090/".to_string()
}

fn bind_address() -> String {
	"127.0.0.1:8080".to_string()
}

fn domain_allowlist_default() -> Vec<String> {
	vec![]
}

async fn query_uptime(domain: &str) -> Result<UptimeResponse, prometheus_http_query::error::Error> {
	const NDAYS: u64 = 14;

	let client = Client::try_from(CONFIG.prometheus_url.clone())?;
	let q = format!(
		"max(avg_over_time(probe_success{{job=~\"xmppobserve:xmpps?-(client|server)\", domain=\"zombofant.net\"}}[1h])) by (domain)",
	);
	let t1 = std::time::SystemTime::now()
		.duration_since(std::time::SystemTime::UNIX_EPOCH)
		.unwrap()
		.as_secs();
	let t1 = t1 - (t1 % 3600);
	let t0 = t1 - 3600 * 24 * NDAYS;

	let response = client
		.query_range(q, t0 as i64, t1 as i64, 3600.0)
		.get()
		.await?;
	let series = response.data().as_matrix().expect("matrix result");
	let mut samples = Vec::new();
	samples.resize(24 * NDAYS as usize + 1, None);
	for sample in series[0].samples() {
		let bucket = ((sample.timestamp() - t0 as f64) as i64) / 3600;
		if bucket < 0 {
			continue;
		}
		if let Some(dest) = samples.get_mut(bucket as usize) {
			*dest = Some(sample.value());
		}
	}
	Ok(UptimeResponse {
		domain: domain.into(),
		t0,
		uptime_history: samples,
	})
}

#[handler]
async fn uptime(Path(domain): Path<String>) -> (StatusCode, Json<Response>) {
	if !CONFIG.domain_allowlist.contains(&domain) {
		return (
			StatusCode::NOT_FOUND,
			Json(Response::Error(ErrorResponse {
				message: format!("domain {} is not tracked", domain),
			})),
		);
	}

	match query_uptime(&domain).await {
		Ok(v) => (StatusCode::OK, Json(Response::Success(v))),
		Err(e) => (
			StatusCode::INTERNAL_SERVER_ERROR,
			Json(Response::Error(ErrorResponse {
				message: e.to_string(),
			})),
		),
	}
}

#[tokio::main]
async fn main() -> Result<(), Box<(dyn std::error::Error + 'static)>> {
	let app = Route::new().at("/uptime/:domain", get(uptime));
	Server::new(TcpListener::bind(&CONFIG.bind_address))
		.run(app)
		.await?;
	Ok(())
}
