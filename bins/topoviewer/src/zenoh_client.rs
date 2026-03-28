//! Zenoh client — subscribes to backend metrics and bridges to Iced messages.

use std::pin::Pin;

use iced::futures::stream::unfold;
use iced::futures::Stream;
use iced::Subscription;

use nlink_lab_shared::metrics::MetricsSnapshot;

use crate::app::Message;

/// Create a Zenoh subscription for live metrics from a lab.
pub fn metrics_subscription(lab_name: String) -> Subscription<Message> {
    Subscription::run_with(lab_name, create_stream)
}

struct Connected {
    #[allow(dead_code)]
    session: zenoh::Session,
    subscriber:
        zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>,
}

enum State {
    Starting(String),
    Connected(Connected),
}

fn create_stream(lab_name: &String) -> Pin<Box<dyn Stream<Item = Message> + Send>> {
    let lab = lab_name.clone();
    Box::pin(unfold(State::Starting(lab), |state| async move {
        match state {
            State::Starting(lab) => {
                let session = match zenoh::open(zenoh::Config::default()).await {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("Zenoh connect failed: {e}");
                        return None;
                    }
                };

                let topic = nlink_lab_shared::topics::metrics_snapshot(&lab);
                match session.declare_subscriber(&topic).await {
                    Ok(sub) => {
                        let connected = Connected {
                            session,
                            subscriber: sub,
                        };
                        Some((
                            Message::MetricsReceived(Default::default()),
                            State::Connected(connected),
                        ))
                    }
                    Err(e) => {
                        eprintln!("Zenoh subscribe failed: {e}");
                        None
                    }
                }
            }
            State::Connected(conn) => match conn.subscriber.recv_async().await {
                Ok(sample) => {
                    let payload = sample.payload().to_bytes();
                    let msg = if let Ok(snapshot) =
                        serde_json::from_slice::<MetricsSnapshot>(&payload)
                    {
                        Message::MetricsReceived(snapshot.nodes)
                    } else {
                        Message::MetricsReceived(Default::default())
                    };
                    Some((msg, State::Connected(conn)))
                }
                Err(_) => None,
            },
        }
    }))
}
