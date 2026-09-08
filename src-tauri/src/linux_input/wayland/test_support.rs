//! In-process EIS peer for tests. Uses only a socket pair, never the desktop,
//! session D-Bus, or a real portal permission dialog.

use std::{os::unix::net::UnixStream, time::Duration};

use futures_util::StreamExt;
use reis::{
    ei, eis,
    event::Connection,
    handshake::EisHandshaker,
    request::{self, DeviceCapability, EisRequest, EisRequestConverter},
    tokio::EiConvertEventStream,
    PendingRequestResult,
};

use super::{
    eis::{EisInput, EventEffect},
    geometry::{match_monitors, PortalMonitor},
};
use crate::Screen;

pub(super) struct MockEis {
    context: eis::Context,
    converter: EisRequestConverter,
    seat: request::Seat,
    pub keyboard: Option<request::Device>,
    pub pointer: Option<request::Device>,
}

impl MockEis {
    pub async fn connect() -> (Self, Connection, EiConvertEventStream) {
        let (client, server) = UnixStream::pair().unwrap();
        let client = ei::Context::new(client).unwrap();
        let server = eis::Context::new(server).unwrap();
        let mut handshaker = EisHandshaker::new(&server, 1);
        let server_handshake = async {
            loop {
                server.read().unwrap();
                while let Some(request) = server.pending_request() {
                    let PendingRequestResult::Request(request) = request else {
                        panic!("invalid handshake request");
                    };
                    if let Some(response) = handshaker.handle_request(request).unwrap() {
                        server.flush().unwrap();
                        return EisRequestConverter::new(&server, response, 1);
                    }
                }
                server.flush().unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        let (client_result, converter) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                client.handshake_tokio("MyKVM-test", ei::handshake::ContextType::Sender),
                server_handshake
            )
        })
        .await
        .unwrap();
        let (connection, events) = client_result.unwrap();
        let seat = converter.handle().add_seat(
            Some("test"),
            DeviceCapability::Keyboard
                | DeviceCapability::PointerAbsolute
                | DeviceCapability::Button
                | DeviceCapability::Scroll,
        );
        converter.handle().flush().unwrap();
        (
            Self {
                context: server,
                converter,
                seat,
                keyboard: None,
                pointer: None,
            },
            connection,
            events,
        )
    }

    pub fn add_pointer(&mut self) {
        let device = self.seat.add_device(
            Some("pointer"),
            eis::device::DeviceType::Virtual,
            DeviceCapability::PointerAbsolute | DeviceCapability::Button | DeviceCapability::Scroll,
            |device| {
                device.device().region_mapping_id("primary");
                device.device().region(0, 0, 1920, 1080, 2.0);
            },
        );
        device.resumed();
        self.pointer = Some(device);
        self.flush();
    }

    pub fn add_keyboard(&mut self) {
        let device = self.seat.add_device(
            Some("keyboard"),
            eis::device::DeviceType::Virtual,
            DeviceCapability::Keyboard.into(),
            |_| {},
        );
        device.resumed();
        self.keyboard = Some(device);
        self.flush();
    }

    pub fn add_keyboard_with_button(&mut self) {
        let device = self.seat.add_device(
            Some("mixed-keyboard"),
            eis::device::DeviceType::Virtual,
            DeviceCapability::Keyboard | DeviceCapability::Button,
            |_| {},
        );
        device.resumed();
        self.keyboard = Some(device);
        self.flush();
    }

    pub fn add_foreign_keyboard(&self) {
        let seat = self
            .converter
            .handle()
            .add_seat(Some("unbound-seat"), DeviceCapability::Keyboard.into());
        let device = seat.add_device(
            Some("foreign-keyboard"),
            eis::device::DeviceType::Virtual,
            DeviceCapability::Keyboard.into(),
            |_| {},
        );
        device.resumed();
        self.flush();
    }

    pub fn flush(&self) {
        self.converter.handle().flush().unwrap();
    }

    pub fn requests(&mut self) -> Vec<EisRequest> {
        if let Err(error) = self.context.read() {
            // Cancelling before a queued resume is consumed can close a Unix
            // socket with unread peer data: Linux reports ECONNRESET, not EOF.
            assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                ),
                "unexpected mock EIS socket error: {error}"
            );
        }
        while let Some(request) = self.context.pending_request() {
            let PendingRequestResult::Request(request) = request else {
                panic!("invalid EIS request");
            };
            self.converter.handle_request(request).unwrap();
        }
        std::iter::from_fn(|| self.converter.next_request()).collect()
    }
}

pub(super) fn input(connection: Connection) -> EisInput {
    let screen = Screen {
        id: "screen".into(),
        device_id: "local".into(),
        name: "primary".into(),
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
        scale: 2.0,
        is_primary: true,
    };
    let monitors = match_monitors(
        &[screen],
        &[PortalMonitor {
            position: Some((0, 0)),
            size: Some((1920, 1080)),
            mapping_id: Some("primary".into()),
        }],
    )
    .unwrap();
    EisInput::new(connection, monitors)
}

pub(super) async fn event(
    input: &mut EisInput,
    events: &mut EiConvertEventStream,
) -> Result<EventEffect, String> {
    let event = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let effect = input.event(event)?;
    input.flush()?;
    Ok(effect)
}

pub(super) async fn ready() -> (MockEis, EisInput, EiConvertEventStream) {
    let (mut server, connection, mut events) = MockEis::connect().await;
    let mut input = input(connection);
    event(&mut input, &mut events).await.unwrap(); // seat + bind
    assert!(server
        .requests()
        .iter()
        .any(|request| matches!(request, EisRequest::Bind(_))));
    server.add_pointer();
    server.add_keyboard();
    while input.prepare().is_err() {
        event(&mut input, &mut events).await.unwrap();
    }
    let starts = server
        .requests()
        .into_iter()
        .filter(|request| matches!(request, EisRequest::DeviceStartEmulating(_)))
        .count();
    assert_eq!(starts, 2);
    (server, input, events)
}
