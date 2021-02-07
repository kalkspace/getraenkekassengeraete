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

mod barcodeservice;
mod nfcservice;

/// Our global unique client id counter.
static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);

// this is a generic type right now and I don't like it but I fail
// to send two different messages because of rust n00bness
#[derive(Debug, Clone, Serialize)]
struct Message<'a> {
    r#type: &'a str,
    id: String,
}

/// Our state of currently connected users.
///
/// - Key is their id
/// - Value is a sender of `Message`
type Clients<'a> = Arc<Mutex<HashMap<usize, mpsc::UnboundedSender<Message<'a>>>>>;

async fn consume_device_events<'a>(
    clients: Clients<'a>,
    nfc_stream: impl Stream<Item = Option<nfcservice::CardDetail>>,
    barcode_stream: impl Stream<Item = String>,
) {
    tokio::pin!(nfc_stream);
    tokio::pin!(barcode_stream);
    loop {
        let message = tokio::select! {
            nfc = nfc_stream.next() => {
                // too much nesting :S
                // unsure why there is another option
                if nfc.is_none() {
                    continue;
                }
                let nfc = nfc.unwrap();
                match nfc {
                    None => Message{
                        r#type: "nfc-invalid",
                        id: String::new(),
                    },
                    Some(card_detail) => {
                        match card_detail {
                            nfcservice::CardDetail::MeteUuid(uuid) => {
                                Message{
                                r#type: "nfc-uuid",
                                id: uuid,
                            }},
                            nfcservice::CardDetail::Plain(uid) => {
                                Message{
                                r#type: "nfc-plain",
                                id: uid.iter().map(|x| format!("{:02x}", x)).collect::<String>(),
                            }},
                            nfcservice::CardDetail::GetraenkeKarteMissing => {
                                Message{
                                    r#type: "nfc-missing-getraenkekarte",
                                    id: String::new(),
                                }
                            }
                        }
                    }
                }
            },
            barcode = barcode_stream.next() => {
                if barcode.is_none() {
                    continue;
                }
                let barcode = barcode.unwrap();
                Message {
                    r#type: "barcode",
                    id: barcode,
                }
            }
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
    // hardcoded for now
    let barcode_stream =
        barcodeservice::run("/dev/input/by-id/usb-Newtologic_NT4010S_XXXXXX-event-kbd");

    // Keep track of all connected clients, key is usize, value
    // is an event stream sender.

    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let cloned_clients = clients.clone();
    tokio::spawn(async move {
        consume_device_events(cloned_clients, nfc_stream, barcode_stream).await;
    });

    // failing to make this optional
    let allow_origin = std::env::var("ALLOW_ORIGIN").unwrap_or(String::from("https://example.com"));

    // Turn our "state" into a new Filter...
    let clients = warp::any().map(move || clients.clone());
    let route = warp::path::end()
        .and(warp::get())
        .and(clients)
        .map(|clients| {
            let stream = cashier_event_stream(clients);
            // reply using server-sent events
            warp::sse::reply(warp::sse::keep_alive().stream(stream))
        })
        .with(
            warp::cors()
                .allow_origin(allow_origin.as_str())
                .allow_methods(vec!["GET", "POST", "DELETE"]),
        );

    let addr = match std::env::var("BIND") {
        Ok(var) => var,
        Err(_) => String::from("0.0.0.0:3030"),
    };

    warp::serve(route).run(addr.parse::<SocketAddr>()?).await;
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
            .data(serde_json::to_string(&msg.id).unwrap()))
    })
}
