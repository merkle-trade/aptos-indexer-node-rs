#![deny(clippy::all)]

use aptos_protos::indexer::v1::{
  raw_data_client::RawDataClient, GetTransactionsRequest, TransactionsResponse,
};
use aptos_protos::transaction::v1::transaction::TxnData;
use aptos_protos::transaction::v1::write_set_change::Change;
use futures::StreamExt;
use lazy_static::lazy_static;
use napi::bindgen_prelude::BigInt;
use std::collections::HashMap;
use std::sync::{atomic::AtomicI32, Arc};
use tokio::sync::{mpsc, Mutex};

#[macro_use]
extern crate napi_derive;

const MAX_RESPONSE_SIZE: usize = 1024 * 1024 * 256;
const MPSC_BUFFER_SIZE: usize = 5;

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
pub struct TransactionFilter {
  // only transactions with events from these addresses will be returned
  pub focus_contract_addresses: Vec<String>,
}

#[napi]
pub async fn start_fetch_transactions(
  url: String,
  auth_key: Option<String>,
  start_version: BigInt,
  end_version: Option<BigInt>,
  filter: Option<TransactionFilter>,
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

  RUNTIME.spawn(async move { filter_txs(fetch_rx, filter_tx, filter).await });

  ch
}

#[napi]
pub async fn next_transactions(ch: i32) -> String {
  let mut rxs = RXS.lock().await;
  let rx = rxs.get_mut(&ch).unwrap();

  let res = rx.recv().await;

  serde_json::to_string(&res).unwrap()
}

async fn fetch_txs(
  tx: mpsc::Sender<TransactionsResponse>,
  url: String,
  auth_key: Option<String>,
  start_version: u64,
  end_version: Option<u64>,
) {
  let mut client = RawDataClient::connect(url)
    .await
    .unwrap()
    .accept_compressed(tonic::codec::CompressionEncoding::Gzip)
    .accept_compressed(tonic::codec::CompressionEncoding::Zstd)
    .send_compressed(tonic::codec::CompressionEncoding::Zstd)
    .max_decoding_message_size(MAX_RESPONSE_SIZE)
    .max_encoding_message_size(MAX_RESPONSE_SIZE);

  println!(
    "start_version: {}, end_version: {:?}",
    start_version, end_version
  );

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

  let mut response = client.get_transactions(request).await.unwrap().into_inner();

  loop {
    let r = response.next().await.unwrap().unwrap();
    if r.transactions.len() == 0 {
      continue;
    }

    tx.send(r).await.unwrap();
  }
}

async fn filter_txs(
  mut rx: mpsc::Receiver<TransactionsResponse>,
  tx: mpsc::Sender<TransactionsResponse>,
  filter: Option<TransactionFilter>,
) {
  let focus_contract_addresses = filter
    .map(|f| f.focus_contract_addresses)
    .unwrap_or_default();

  let is_focus_type = |t: &str| {
    focus_contract_addresses
      .iter()
      .any(|a| t.starts_with(&format!("{}::", a)))
  };

  loop {
    let mut r = rx.recv().await.unwrap();
    r.transactions = r
      .transactions
      .into_iter()
      .filter_map(|mut txn| {
        if let Some(TxnData::User(user_txn)) = txn.txn_data.as_mut() {
          let is_focus = user_txn.events.iter().any(|e| is_focus_type(&e.type_str));
          if is_focus {
            return Some(txn);
          }

          let is_focus = if let Some(info) = txn.info.as_ref() {
            info.changes.iter().any(|c| match c.change.as_ref() {
              Some(Change::DeleteResource(dr)) => is_focus_type(dr.type_str.as_str()),
              Some(Change::DeleteTableItem(_)) => true,
              Some(Change::WriteResource(wr)) => is_focus_type(wr.type_str.as_str()),
              Some(Change::WriteTableItem(wti)) => is_focus_type(
                wti
                  .data
                  .as_ref()
                  .map(|d| d.value_type.as_str())
                  .unwrap_or_default(),
              ),
              _ => false,
            })
          } else {
            false
          };
          if is_focus {
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
        }
        None
      })
      .collect();

    tx.send(r).await.unwrap();
  }
}

#[napi]
pub fn sum(a: i32, b: i32) -> i32 {
  a + b
}
