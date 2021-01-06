use crate::{
    buffers::Acker,
    config::{log_schema, DataType, GenerateConfig, SinkConfig, SinkContext, SinkDescription},
    event::Event,
    internal_events::{
        RedisEventEncodeError, RedisEventSend, RedisEventSendFail, RedisMissingKeys,
    },
    sinks::util::encoding::{EncodingConfig, EncodingConfigWithDefault, EncodingConfiguration},
    template::{Template, TemplateError},
};

use futures::{future::BoxFuture, ready, stream::FuturesUnordered, FutureExt, Sink, Stream};
use redis::{aio::ConnectionManager, AsyncCommands, RedisError, RedisResult};
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::{
    collections::HashSet,
    convert::TryFrom,
    pin::Pin,
    task::{Context, Poll},
};

#[derive(Clone, Debug, Derivative, Deserialize, Serialize)]
#[derivative(Default)]
#[serde(rename_all = "lowercase")]
pub enum Type {
    #[derivative(Default)]
    List,
    Channel,
}

#[derive(Clone, Copy, Debug, Derivative, Deserialize, Serialize, Eq, PartialEq)]
#[derivative(Default)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    #[derivative(Default)]
    LPUSH,
    RPUSH,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RedisSinkConfig {
    #[serde(default)]
    pub data_type: Type,
    pub method: Option<Method>,
    pub encoding: EncodingConfigWithDefault<Encoding>,
    pub url: String,
    pub key: String,
}

#[derive(Clone, Copy, Debug, Derivative, Deserialize, Serialize, Eq, PartialEq)]
#[derivative(Default)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    #[derivative(Default)]
    Json,
    Text,
}

enum RedisSinkState {
    None,
    Ready(ConnectionManager),
    Sending(BoxFuture<'static, (ConnectionManager, Result<i32, RedisError>)>),
}

#[derive(Debug, Snafu)]
enum ParseError {
    #[snafu(display("Key template parse error: {}", source))]
    KeyTemplate { source: TemplateError },
}

pub struct RedisSink {
    key: Template,
    data_type: Type,
    method: Option<Method>,
    encoding: EncodingConfig<Encoding>,
    state: RedisSinkState,
    in_flight: FuturesUnordered<BoxFuture<'static, (usize, Result<i32, RedisError>)>>,
    acker: Acker,
    seq_head: usize,
    seq_tail: usize,
    pending_acks: HashSet<usize>,
}

inventory::submit! {
    SinkDescription::new::<RedisSinkConfig>("redis")
}

impl GenerateConfig for RedisSinkConfig {
    fn generate_config() -> toml::Value {
        toml::Value::try_from(&Self {
            url: "redis://127.0.0.1:6379/0".to_owned(),
            key: "vector".to_owned(),
            encoding: Encoding::Json.into(),
            data_type: Type::List,
            method: Option::from(Method::LPUSH),
        })
        .unwrap()
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "redis")]
impl SinkConfig for RedisSinkConfig {
    async fn build(
        &self,
        cx: SinkContext,
    ) -> crate::Result<(super::VectorSink, super::Healthcheck)> {
        if self.key.is_empty() {
            return Err("`key` cannot be empty.".into());
        } else if let Type::List = self.data_type {
            if self.method.is_none() {
                return Err("When `data_type` is `list`, `method` cannot be empty.".into());
            }
        }

        let sink = RedisSink::new(self.clone(), cx.acker()).await?;

        let conn = match &(sink.state) {
            RedisSinkState::Ready(conn) => conn.clone(),
            _ => unreachable!(),
        };
        let healthcheck = healthcheck(conn).boxed();

        Ok((super::VectorSink::Sink(Box::new(sink)), healthcheck))
    }

    fn input_type(&self) -> DataType {
        DataType::Log
    }

    fn sink_type(&self) -> &'static str {
        "redis"
    }
}

impl RedisSinkConfig {
    async fn build_client(&self) -> RedisResult<ConnectionManager> {
        trace!("Open redis client.");
        let client = redis::Client::open(self.url.as_str())?;
        trace!("Open redis client success.");
        trace!("Get redis connection.");
        let conn = client.get_tokio_connection_manager().await;
        trace!("Get redis connection success.");
        conn
    }
}

impl RedisSink {
    async fn new(config: RedisSinkConfig, acker: Acker) -> crate::Result<Self> {
        let res = config.build_client().await;

        let key_tmpl = Template::try_from(config.key).context(KeyTemplate)?;

        match res {
            Ok(conn) => Ok(RedisSink {
                data_type: config.data_type,
                method: config.method,
                key: key_tmpl,
                encoding: config.encoding.into(),
                acker,
                seq_head: 0,
                seq_tail: 0,
                pending_acks: HashSet::new(),
                in_flight: FuturesUnordered::new(),
                state: RedisSinkState::Ready(conn),
            }),
            Err(error) => {
                error!(message = "Redis sink init generated an error.", %error);
                Err(error.to_string().into())
            }
        }
    }
    fn poll_in_flight_prepare(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if let RedisSinkState::Sending(fut) = &mut self.state {
            let (conn, result) = ready!(fut.as_mut().poll(cx));

            let seqno = self.seq_head;
            self.seq_head += 1;

            self.state = RedisSinkState::Ready(conn);
            self.in_flight
                .push(Box::pin(async move { (seqno, result) }));
        }
        Poll::Ready(())
    }
}

impl Sink<Event> for RedisSink {
    type Error = ();

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.poll_in_flight_prepare(cx));
        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, event: Event) -> Result<(), Self::Error> {
        assert!(
            matches!(self.state, RedisSinkState::Ready(_)),
            "expected `poll_ready` to be called first."
        );

        let conn = match std::mem::replace(&mut self.state, RedisSinkState::None) {
            RedisSinkState::Ready(conn) => conn,
            _ => unreachable!(),
        };

        let key = self.key.render_string(&event).map_err(|missing_keys| {
            emit!(RedisMissingKeys {
                keys: &missing_keys
            });
        })?;

        let encoded = encode_event(event, &self.encoding);
        let message_len = encoded.len();

        match self.data_type {
            Type::List => match self.method {
                Some(Method::LPUSH) => {
                    let _ = std::mem::replace(
                        &mut self.state,
                        RedisSinkState::Sending(Box::pin(async move {
                            let result = lpush(conn.clone(), key.clone(), encoded.clone()).await;
                            if result.is_ok() {
                                emit!(RedisEventSend {
                                    byte_size: message_len,
                                });
                            }
                            (conn, result)
                        })),
                    );
                }
                Some(Method::RPUSH) => {
                    let _ = std::mem::replace(
                        &mut self.state,
                        RedisSinkState::Sending(Box::pin(async move {
                            let result = rpush(conn.clone(), key.clone(), encoded.clone()).await;
                            if result.is_ok() {
                                emit!(RedisEventSend {
                                    byte_size: message_len,
                                });
                            }
                            (conn, result)
                        })),
                    );
                }
                None => {
                    panic!("When `data_type` is `list`, `method` cannot be empty.")
                }
            },
            Type::Channel => {
                let _ = std::mem::replace(
                    &mut self.state,
                    RedisSinkState::Sending(Box::pin(async move {
                        let result = publish(conn.clone(), key.clone(), encoded.clone()).await;
                        if result.is_ok() {
                            emit!(RedisEventSend {
                                byte_size: message_len,
                            });
                        }
                        (conn, result)
                    })),
                );
            }
        }
        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.poll_in_flight_prepare(cx));

        let this = Pin::into_inner(self);
        while !this.in_flight.is_empty() {
            match ready!(Pin::new(&mut this.in_flight).poll_next(cx)) {
                Some((seqno, Ok(result))) => {
                    trace!(
                        message = "Redis sink produced message.",
                        length = ?result,
                    );
                    this.pending_acks.insert(seqno);
                    let mut num_to_ack = 0;
                    while this.pending_acks.remove(&this.seq_tail) {
                        num_to_ack += 1;
                        this.seq_tail += 1
                    }
                    this.acker.ack(num_to_ack);
                }
                Some((_, Err(error))) => {
                    error!(message = "Redis sink generated an error.", %error);
                    emit!(RedisEventSendFail { error });
                    return Poll::Ready(Err(()));
                }
                None => break,
            }
        }
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }
}

async fn lpush(mut conn: ConnectionManager, key: String, value: String) -> Result<i32, RedisError> {
    let res: RedisResult<i32> = conn.lpush(key, value).await;
    match res {
        Ok(len) => Ok(len),
        Err(e) => Err(e),
    }
}

async fn rpush(mut conn: ConnectionManager, key: String, value: String) -> Result<i32, RedisError> {
    let res: RedisResult<i32> = conn.rpush(key, value).await;
    match res {
        Ok(len) => Ok(len),
        Err(e) => Err(e),
    }
}

async fn publish(
    mut conn: ConnectionManager,
    key: String,
    value: String,
) -> Result<i32, RedisError> {
    let res: RedisResult<i32> = conn.publish(key, value).await;
    match res {
        Ok(len) => Ok(len),
        Err(e) => Err(e),
    }
}

async fn healthcheck(mut conn: ConnectionManager) -> crate::Result<()> {
    redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .map_err(Into::into)
}

fn encode_event(mut event: Event, encoding: &EncodingConfig<Encoding>) -> String {
    encoding.apply_rules(&mut event);

    match encoding.codec() {
        Encoding::Json => serde_json::to_string(event.as_log())
            .map_err(|error| emit!(RedisEventEncodeError { error }))
            .unwrap_or_default(),
        Encoding::Text => event
            .as_log()
            .get(log_schema().message_key())
            .map(|v| v.to_string_lossy())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn generate_config() {
        crate::test_util::test_generate_config::<RedisSinkConfig>();
    }

    #[test]
    fn redis_event_json() {
        let msg = "hello_world".to_owned();
        let mut evt = Event::from(msg.clone());
        evt.as_mut_log().insert("key", "value");
        let result = encode_event(evt, &EncodingConfig::from(Encoding::Json));
        let map: HashMap<String, String> = serde_json::from_str(result.as_str()).unwrap();
        assert_eq!(msg, map[&log_schema().message_key().to_string()]);
    }

    #[test]
    fn redis_event_text() {
        let msg = "hello_world".to_owned();
        let evt = Event::from(msg.clone());
        let event = encode_event(evt, &EncodingConfig::from(Encoding::Text));
        assert_eq!(event, msg);
    }

    #[test]
    fn redis_encode_event() {
        let msg = "hello_world";
        let mut evt = Event::from(msg);
        evt.as_mut_log().insert("key", "value");

        let event = encode_event(
            evt,
            &EncodingConfigWithDefault {
                codec: Encoding::Json,
                except_fields: Some(vec!["key".into()]),
                ..Default::default()
            }
            .into(),
        );

        let map: HashMap<String, String> = serde_json::from_str(event.as_str()).unwrap();
        assert!(!map.contains_key("key"));
    }
}

#[cfg(feature = "redis-integration-tests")]
#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::test_util::{random_lines_with_stream, random_string, trace_init};
    use futures::StreamExt;
    use rand::Rng;

    const REDIS_SERVER: &str = "redis://127.0.0.1:6379/0";

    #[tokio::test]
    async fn redis_sink_list_lpush() {
        trace_init();

        let key = format!("test-{}", random_string(10));
        debug!("Test key name: {}.", key);
        let mut rng = rand::thread_rng();
        let num_events = rng.gen_range(1000, 2000);
        debug!("Test events num: {}.", num_events);

        let cnf = RedisSinkConfig {
            url: REDIS_SERVER.to_owned(),
            key: key.clone(),
            encoding: Encoding::Json.into(),
            data_type: Type::List,
            method: Option::from(Method::LPUSH),
        };

        // Publish events.
        let (acker, ack_counter) = Acker::new_for_testing();
        let sink = RedisSink::new(cnf.clone(), acker).await.unwrap();
        let (_input, events) = random_lines_with_stream(1000, num_events);
        events.map(Ok).forward(sink).await.unwrap();

        assert_eq!(
            ack_counter.load(std::sync::atomic::Ordering::Relaxed),
            num_events
        );

        let mut conn = cnf.build_client().await.unwrap();

        let key_exists: bool = conn.exists(key.clone()).await.unwrap();
        debug!("Test key: {} exists: {}.", key, key_exists);
        assert_eq!(key_exists, true);
        let llen: usize = conn.llen(key.clone()).await.unwrap();
        debug!("Test key: {} len: {}.", key, llen);
        assert_eq!(llen, num_events);
    }

    #[tokio::test]
    async fn redis_sink_list_rpush() {
        trace_init();

        let key = format!("test-{}", random_string(10));
        debug!("Test key name: {}.", key);
        let mut rng = rand::thread_rng();
        let num_events = rng.gen_range(1000, 2000);
        debug!("Test events num: {}.", num_events);

        let cnf = RedisSinkConfig {
            url: REDIS_SERVER.to_owned(),
            key: key.clone(),
            encoding: Encoding::Json.into(),
            data_type: Type::List,
            method: Option::from(Method::RPUSH),
        };

        // Publish events.
        let (acker, ack_counter) = Acker::new_for_testing();
        let sink = RedisSink::new(cnf.clone(), acker).await.unwrap();
        let (_input, events) = random_lines_with_stream(100, num_events);
        events.map(Ok).forward(sink).await.unwrap();

        assert_eq!(
            ack_counter.load(std::sync::atomic::Ordering::Relaxed),
            num_events
        );

        let mut conn = cnf.build_client().await.unwrap();

        let key_exists: bool = conn.exists(key.clone()).await.unwrap();
        debug!("Test key: {} exists: {}.", key, key_exists);
        assert_eq!(key_exists, true);
        let llen: usize = conn.llen(key.clone()).await.unwrap();
        debug!("Test key: {} len: {}.", key, llen);
        assert_eq!(llen, num_events);
    }

    #[tokio::test]
    async fn redis_sink_channel() {
        trace_init();

        let key = format!("test-{}", random_string(10));
        debug!("Test key name: {}.", key);
        let mut rng = rand::thread_rng();
        let num_events = rng.gen_range(1000, 2000);
        debug!("Test events num: {}.", num_events);

        let client = redis::Client::open(REDIS_SERVER).unwrap();
        debug!("Get redis async connection.");
        let conn = client
            .get_async_connection()
            .await
            .expect("Failed to get redis async connection.");
        debug!("Get redis async connection success.");
        let mut pubsub_conn = conn.into_pubsub();
        debug!("Subscrib channel:{}.", key.as_str());
        pubsub_conn
            .subscribe(key.as_str())
            .await
            .unwrap_or_else(|_| panic!("Failed to subscribe channel:{}.", key.as_str()));
        debug!("Subscribed to channel:{}.", key.as_str());
        let mut pubsub_stream = pubsub_conn.on_message();

        let cnf = RedisSinkConfig {
            url: REDIS_SERVER.to_owned(),
            key: key.clone(),
            encoding: Encoding::Json.into(),
            data_type: Type::Channel,
            method: None,
        };

        // Publish events.
        let (acker, ack_counter) = Acker::new_for_testing();
        let sink = RedisSink::new(cnf.clone(), acker).await.unwrap();
        let (_input, events) = random_lines_with_stream(100, num_events);
        events.map(Ok).forward(sink).await.unwrap();

        assert_eq!(
            ack_counter.load(std::sync::atomic::Ordering::Relaxed),
            num_events
        );

        // Receive events.
        let mut received_msg_num = 0;
        loop {
            let _msg = pubsub_stream.next().await.unwrap();
            received_msg_num += 1;
            debug!("Received msg num:{}.", received_msg_num);
            if received_msg_num == num_events {
                assert_eq!(received_msg_num, num_events);
                break;
            }
        }
    }
}
