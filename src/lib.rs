#![deny(clippy::all)]

use aptos_protos::indexer::v1::{
  raw_data_client::RawDataClient, GetTransactionsRequest, TransactionsResponse,
};
use aptos_protos::transaction::v1::transaction::TxnData;
use aptos_protos::transaction::v1::write_set_change::Change;
use futures::StreamExt;
use lazy_static::lazy_static;
use napi::bindgen_prelude::BigInt;
use pcre2::bytes::Regex;
use std::collections::HashMap;
use std::sync::{atomic::AtomicI32, Arc};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};

#[macro_use]
extern crate napi_derive;

const MAX_RESPONSE_SIZE: usize = 1024 * 1024 * 256;
const MPSC_BUFFER_SIZE: usize = 10;

lazy_static! {
  static ref RUNTIME: tokio::runtime::Runtime = tokio::runtime::Builder::new_multi_thread()
    .disable_lifo_slot()
    .enable_all()
    .worker_threads({
      let num_cpus = num_cpus::get();
      let worker_threads = num_cpus.max(16);
      worker_threads
    })
    .build()
    .unwrap();
  static ref RXS: Arc<Mutex<HashMap<i32, mpsc::Receiver<TransactionsResponse>>>> =
    Arc::new(Mutex::new(HashMap::new()));
  static ref CHANNEL_ID: AtomicI32 = AtomicI32::new(0);
}

#[napi(object)]
#[derive(Default)]
pub struct TransactionFilterOptions {
  pub type_patterns: Option<Vec<String>>,
  pub fetch_all_delete_table_item: Option<bool>,
}

#[napi]
pub async fn start_fetch_transactions(
  url: String,
  auth_key: Option<String>,
  start_version: BigInt,
  end_version: Option<BigInt>,
  filter_options: Option<TransactionFilterOptions>,
) -> i32 {
  let (fetch_tx, fetch_rx) = mpsc::channel(MPSC_BUFFER_SIZE);
  let (filter_tx, filter_rx) = mpsc::channel(MPSC_BUFFER_SIZE);
  let ch = CHANNEL_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
  let mut rxs = RXS.lock().await;
  rxs.insert(ch, filter_rx);

  RUNTIME.spawn(async move {
    fetch_txs(
      fetch_tx,
      url,
      auth_key,
      start_version.get_u64().1,
      end_version.map(|v| v.get_u64().1),
    )
    .await
  });

  RUNTIME.spawn(async move { filter_txs(fetch_rx, filter_tx, filter_options).await });

  ch
}

#[napi]
pub async fn next_transactions(ch: i32) -> Option<String> {
  let mut rxs = RXS.lock().await;
  let rx = rxs.get_mut(&ch)?;
  let res = rx.recv().await?;
  serde_json::to_string(&res).ok() // TODO: find better way to pass data to JS
}

async fn get_fetch_txs_stream(
  url: String,
  auth_key: Option<String>,
  start_version: u64,
  end_version: Option<u64>,
) -> tonic::Streaming<TransactionsResponse> {
  let mut client = RawDataClient::connect(url)
    .await
    .unwrap()
    .accept_compressed(tonic::codec::CompressionEncoding::Gzip)
    .accept_compressed(tonic::codec::CompressionEncoding::Zstd)
    .send_compressed(tonic::codec::CompressionEncoding::Zstd)
    .max_decoding_message_size(MAX_RESPONSE_SIZE)
    .max_encoding_message_size(MAX_RESPONSE_SIZE);

  let count = end_version.map(|v| v - start_version + 1);
  let mut request = tonic::Request::new(GetTransactionsRequest {
    starting_version: Some(start_version),
    batch_size: None,
    transactions_count: count,
  });

  if let Some(auth_key) = auth_key {
    request.metadata_mut().insert(
      "authorization",
      format!("Bearer {}", auth_key).parse().unwrap(),
    );
  }

  let response = client.get_transactions(request).await.unwrap().into_inner();
  response
}

async fn fetch_txs(
  tx: mpsc::Sender<TransactionsResponse>,
  url: String,
  auth_key: Option<String>,
  start_version: u64,
  end_version: Option<u64>,
) {
  println!(
    "start_version: {}, end_version: {:?}",
    start_version, end_version
  );

  let mut last_version = start_version;
  let mut last_err_timestamps = vec![];

  loop {
    let mut stream =
      get_fetch_txs_stream(url.clone(), auth_key.clone(), last_version, end_version).await;
    let end_version = end_version.unwrap_or(std::u64::MAX);

    let is_success = loop {
      match stream.next().await {
        Some(Ok(r)) => {
          let Some(last_tx) = r.transactions.last() else {
            continue;
          };
          let _last_version = last_tx.version;

          if let Err(e) = tx.send(r).await {
            println!("Error: {:?}", e);
            break false;
          }
          last_version = _last_version;
          if last_version >= end_version {
            break true;
          }
        }
        Some(Err(e)) => {
          println!("Error: {:?}", e);
          break false;
        }
        None => {
          println!("End of stream");
          break true;
        }
      }
    };

    let is_all_fetched = last_version >= end_version;
    if is_success && is_all_fetched {
      break;
    }

    // erroneous case. panic if 3 times within 10 seconds
    let now = Instant::now();
    last_err_timestamps = last_err_timestamps
      .into_iter()
      .filter(|t| now.duration_since(*t) < Duration::from_secs(10))
      .collect();
    last_err_timestamps.push(now);
    if last_err_timestamps.len() >= 3 {
      panic!("Too many errors");
    }
  }
}

async fn filter_txs(
  mut rx: mpsc::Receiver<TransactionsResponse>,
  tx: mpsc::Sender<TransactionsResponse>,
  filter_options: Option<TransactionFilterOptions>,
) {
  let TransactionFilterOptions {
    type_patterns,
    fetch_all_delete_table_item,
  } = filter_options.unwrap_or_default();

  let type_patterns = type_patterns
    .map(|ps| {
      ps.into_iter()
        .map(|p| Regex::new(&p).unwrap())
        .collect::<Vec<_>>()
    })
    .unwrap_or_default();
  let fetch_all_delete_table_item = fetch_all_delete_table_item.unwrap_or(false);

  let is_fetch_type = |t: &str| {
    type_patterns.len() == 0
      || type_patterns
        .iter()
        .any(|p| p.is_match(t.as_bytes()).unwrap_or(false))
  };

  while let Some(mut r) = rx.recv().await {
    r.transactions = r
      .transactions
      .into_iter()
      .filter_map(|mut txn| {
        if let Some(TxnData::User(user_txn)) = txn.txn_data.as_mut() {
          let is_fetch = user_txn.events.iter().any(|e| is_fetch_type(&e.type_str));
          if is_fetch {
            return Some(txn);
          }

          let is_fetch = txn.info.as_ref().map_or(false, |info| {
            info.changes.iter().any(|c| match c.change.as_ref() {
              Some(Change::DeleteResource(dr)) => is_fetch_type(&dr.type_str),
              Some(Change::DeleteTableItem(_)) => fetch_all_delete_table_item,
              Some(Change::WriteResource(wr)) => is_fetch_type(&wr.type_str),
              Some(Change::WriteTableItem(wti)) => wti
                .data
                .as_ref()
                .map_or(false, |d| is_fetch_type(&d.value_type)),
              _ => false,
            })
          });
          if is_fetch {
            return Some(txn);
          }

          // strip transaction
          if let Some(info) = txn.info.as_mut() {
            info.changes = vec![];
          }
          user_txn.events = vec![];
          if let Some(request) = user_txn.request.as_mut() {
            request.payload = None;
            request.signature = None;
          }
          return Some(txn);
        }
        None
      })
      .collect();

    if tx.send(r).await.is_err() {
      break;
    }
  }
}

#[napi]
pub fn sum(a: i32, b: i32) -> i32 {
  a + b
}
