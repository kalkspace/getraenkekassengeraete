use axum::extract::State;
use axum::http::{HeaderValue, Method};
use axum::middleware;
use axum::response::sse::{Event, Sse};
use axum::routing::get;
use axum::Router;
use serde::Serialize;
use std::collections::HashMap;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{convert::Infallible, sync::atomic::AtomicUsize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::{Stream, StreamExt as _};
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use getraenkekassengeraete::middlewares::force_local_request;
use getraenkekassengeraete::{barcodeservice, stornoservice};

/// Our global unique client id counter.
static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);

// this is a generic type right now and I don't like it but I fail
// to send two different messages because of rust n00bness
#[derive(Debug, Clone, Serialize)]
struct Message {
    r#type: String,
    id: String,
}

/// Our state of currently connected users.
///
/// - Key is their id
/// - Value is a sender of `Message`
type Clients = Arc<Mutex<HashMap<usize, mpsc::UnboundedSender<Message>>>>;

async fn consume_device_events(
    clients: Clients,
    nfc_stream: impl Stream<Item = Option<nfc_stream::CardDetail>>,
    barcode_stream: impl Stream<Item = String>,
    storno_stream: impl Stream<Item = ()>,
) {
    tokio::pin!(nfc_stream);
    tokio::pin!(barcode_stream);
    tokio::pin!(storno_stream);
    loop {
        let message = tokio::select! {
            nfc = nfc_stream.next() => {
                tracing::debug!("NFC Event: {:?}", nfc);
                // too much nesting :S
                // unsure why there is another option
                if nfc.is_none() {
                    continue;
                }
                let nfc = nfc.unwrap();
                match nfc {
                    None => Message{
                        r#type: "nfc-invalid".to_string(),
                        id: String::new(),
                    },
                    Some(card_detail) => {
                        match card_detail {
                            nfc_stream::CardDetail::MeteUuid(uuid) => {
                                Message{
                                r#type: "nfc-uuid".to_string(),
                                id: uuid,
                            }},
                            nfc_stream::CardDetail::Plain(uid) => {
                                Message{
                                r#type: "nfc-plain".to_string(),
                                id: uid.iter().map(|x| format!("{:02x}", x)).collect::<String>(),
                            }},
                        }
                    }
                }
            },
            barcode = barcode_stream.next() => {
                tracing::debug!("Barcode Event: {:?}", barcode);
                if barcode.is_none() {
                    continue;
                }
                let barcode = barcode.unwrap();
                Message {
                    r#type: "barcode".to_string(),
                    id: barcode,
                }
            },
            storno = storno_stream.next() => {
                tracing::debug!("Storno Event: {:?}", storno);
                if storno.is_none() {
                    continue;
                }
                Message {
                    r#type: "storno".to_string(),
                    id: String::from(""),
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
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Keep track of all connected clients, key is usize, value
    // is an event stream sender.
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let cloned_clients = clients.clone();

    let nfc_stream = nfc_stream::run()?;
    // hardcoded for now
    let barcode_stream =
        barcodeservice::run("/dev/input/by-id/usb-Newtologic_NT4010S_XXXXXX-event-kbd");
    let storno_stream = stornoservice::run("/dev/stornoschluessel");

    tokio::spawn(async move {
        consume_device_events(cloned_clients, nfc_stream, barcode_stream, storno_stream).await;
    });

    let allow_origin = std::env::var("ALLOW_ORIGIN")
        .ok()
        .map(|allow_origin| allow_origin.parse::<HeaderValue>().unwrap());

    // build our application with a route
    let app = Router::new()
        .route("/", get(cashier_event_stream))
        .with_state(clients)
        .layer(ServiceBuilder::new().layer(middleware::from_fn(force_local_request)));

    // there is option_layer() in tower but this changes the error type which mages it incompatible with servicebuilder so add it separately
    let app = match allow_origin {
        Some(allow_origin) => app.layer(
            CorsLayer::new()
                .allow_origin(allow_origin)
                .allow_methods([Method::GET]),
        ),
        None => app,
    };

    let addr = match std::env::var("BIND") {
        Ok(var) => var,
        Err(_) => String::from("[::]:3030"),
    }
    .parse::<SocketAddr>()?;

    tracing::info!("getraenkekassengeraete listening on {}", addr);
    axum::Server::bind(&addr)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;

    Ok(())
}

async fn cashier_event_stream(
    State(clients): State<Clients>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Use a counter to assign a new unique ID for this client.
    let my_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);

    // Use an unbounded channel to handle buffering and flushing of messages
    // to the event source...
    let (tx, rx) = mpsc::unbounded_channel();
    let rx = UnboundedReceiverStream::new(rx);

    // Save the sender in our list of connected users.
    {
        let mut clients = clients.lock().unwrap();
        clients.insert(my_id, tx);
        tracing::debug!("Connected clients: {}", clients.len());
    }

    let stream = rx.map(|msg: Message| {
        Ok(Event::default()
            .event((msg.r#type).clone())
            .data(serde_json::to_string(&msg.id).unwrap()))
    });
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(10))
            .text(""),
    )
}
