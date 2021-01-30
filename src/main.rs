mod nfcservice;

use futures::{Stream, StreamExt};
use serde::Serialize;
use std::collections::HashMap;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::{sse::Event, Filter};

/// Our global unique client id counter.
static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug, Clone)]
struct NfcMessage<'a> {
    r#type: &'a str,
    payload: NfcPayload<'a>,
}

#[derive(Debug, Clone, Serialize)]
struct NfcPayload<'a> {
    id_type: &'a str,
    id: String,
}

/// Our state of currently connected users.
///
/// - Key is their id
/// - Value is a sender of `Message`
type Clients<'a> = Arc<Mutex<HashMap<usize, mpsc::UnboundedSender<NfcMessage<'a>>>>>;

async fn consume_device_events<'a>(
    clients: Clients<'a>,
    nfc_stream: impl Stream<Item = Option<nfcservice::CardDetail>>,
) {
    tokio::pin!(nfc_stream);
    loop {
        let message = tokio::select! {
            nfc = nfc_stream.next() => {
                // too much nesting :S
                let nfc = nfc.unwrap();
                match nfc {
                    None => NfcMessage{
                        r#type: "nfc",
                        payload: NfcPayload {
                        id_type: "invalid",
                        id: String::new(),
                        }
                    },
                    Some(card_detail) => {
                        match card_detail {
                            nfcservice::CardDetail::MeteUuid(uuid) => {
                                NfcMessage{
                                r#type: "nfc",
                                payload: NfcPayload {
                                    id_type: "uuid",
                                    id: uuid,
                                }
                            }},
                            nfcservice::CardDetail::Plain(uid) => {
                                NfcMessage{
                                r#type: "nfc",
                                payload: NfcPayload {
                                    id_type: "plain",
                                    id: uid.iter().map(|x| format!("{:02x}", x)).collect::<String>(),
                                }
                            }},
                        }
                    }
                }
            },
        };
        clients.lock().unwrap().retain(move |_, tx| {
            // If not `is_ok`, the SSE stream is gone, and so don't retain
            tx.send(message.clone()).is_ok()
        });
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();
    let nfc_stream = nfcservice::run()?;

    // Keep track of all connected clients, key is usize, value
    // is an event stream sender.

    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let cloned_clients = clients.clone();
    tokio::spawn(async move {
        consume_device_events(cloned_clients, nfc_stream).await;
    });

    // Turn our "state" into a new Filter...
    let clients = warp::any().map(move || clients.clone());
    let routes = warp::path::end()
        .and(warp::get())
        .and(clients)
        .map(|clients| {
            let stream = cashier_event_stream(clients);
            // reply using server-sent events
            warp::sse::reply(warp::sse::keep_alive().stream(stream))
        });

    let addr = match std::env::var("BIND") {
        Ok(var) => var,
        Err(_) => String::from("0.0.0.0:3030"),
    };

    warp::serve(routes).run(addr.parse::<SocketAddr>()?).await;
    Ok(())
}

fn cashier_event_stream<'a>(
    clients: Clients<'a>,
) -> impl Stream<Item = Result<Event, warp::Error>> + Send + 'a {
    // Use a counter to assign a new unique ID for this client.
    let my_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);

    // Use an unbounded channel to handle buffering and flushing of messages
    // to the event source...
    let (tx, rx) = mpsc::unbounded_channel();
    let rx = UnboundedReceiverStream::new(rx);

    // Save the sender in our list of connected users.
    clients.lock().unwrap().insert(my_id, tx);

    // Convert messages into Server-Sent Events and return resulting stream.
    rx.map(|msg| {
        Ok(Event::default()
            .event((msg.r#type).clone())
            .data(serde_json::to_string(&msg.payload).unwrap()))
    })
}
