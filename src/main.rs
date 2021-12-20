use futures_util::{SinkExt, StreamExt, TryFutureExt};
use serde_derive::{Deserialize, Serialize};
use serde_json::json;
use std::borrow::BorrowMut;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender, UnboundedReceiver, UnboundedSender};
use tokio::sync::RwLock;
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::ws::{Message, WebSocket};
use warp::Filter;

use crossfire::mpmc;

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

#[derive(Deserialize, Serialize)]
struct WhipBody {
    desc: String,
}

type Users = Arc<RwLock<HashMap<usize, mpsc::UnboundedSender<Message>>>>;

static NEXT_USER_ID: AtomicUsize = AtomicUsize::new(1);

#[tokio::main]
async fn main() {
    // Keep track of all connected users, key is usize, value
    // is a websocket sender.
    let users = Users::default();

    let (sdp_tx, sdp_rx) = mpmc::unbounded_future();

    let ws = warp::path("ws")
        // The `ws()` filter will prepare the Websocket handshake.
        .and(warp::ws())
        .and(with_users(users.clone()))
        .and(warp::any().map(move || sdp_tx.clone()))
        .map(
            |ws: warp::ws::Ws, users: Users, sdp_tx: mpmc::TxUnbounded<Message>| {
                // And then our closure will be called when it completes...
                ws.on_upgrade(move |websocket| websocket_connect(websocket, users.clone(), sdp_tx))
            },
        );

    let whip = warp::path("whip")
        .and(warp::post())
        .and(warp::body::json())
        .and(with_users(users.clone()))
        .and(warp::any().map(move || sdp_rx.clone()))
        .and_then(handle_whip);

    let files = warp::get()
        .and(warp::fs::dir("./public/"));

    let routes = ws.or(whip).or(files);

    warp::serve(routes).run(([127, 0, 0, 1], 8000)).await;
}

fn with_users(
    users: Users,
) -> impl Filter<Extract = (Users,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || users.clone())
}

async fn handle_whip(
    whip: WhipBody,
    users: Users,
    sdp_rx: mpmc::RxUnbounded<Message>,
) -> Result<impl warp::Reply, std::convert::Infallible> {
    let message = json!(&whip);
    // New message from this user, send it to everyone else (except same uid)...
    for (&uid, tx) in users.read().await.iter() {
        if let Err(_disconnected) = tx.send(Message::text(message.to_string())) {
            // The tx is disconnected, our `user_disconnected` code
            // should be happening in another task, nothing more to
            // do here.
        }
    }

    if let Ok(res) = sdp_rx.recv().await {
        if let Ok(s) = res.to_str() {
            return Ok(s.to_owned());
        }
    }

    Ok("test".to_string())
}

async fn websocket_connect(
    ws: warp::ws::WebSocket,
    users: Users,
    sdp_tx: mpmc::TxUnbounded<Message>,
) {
    let my_id = NEXT_USER_ID.fetch_add(1, Ordering::Relaxed);

    let (mut user_ws_tx, mut user_ws_rx) = ws.split();

    let (tx, rx) = mpsc::unbounded_channel();
    let mut rx = UnboundedReceiverStream::new(rx);

    tokio::task::spawn(async move {
        while let Some(message) = rx.next().await {
            user_ws_tx
                .send(message)
                .unwrap_or_else(|e| {
                    eprintln!("websocket send error: {}", e);
                })
                .await;
        }
    });

    users.write().await.insert(my_id, tx);

    while let Some(result) = user_ws_rx.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("websocket error: {}", e);
                break;
            }
        };

        sdp_tx.send(msg.clone());

        let s = if let Ok(s) = msg.to_str() {
            s
        } else {
            return;
        };

        println!("{}", s);
    }
}
