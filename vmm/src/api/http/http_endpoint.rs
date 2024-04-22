// Copyright © 2019 Intel Corporation
// Copyright 2024 Alyssa Ross <hi@alyssa.is>
//
// SPDX-License-Identifier: Apache-2.0
//

use crate::api::http::{error_response, EndpointHandler, HttpError};
#[cfg(all(target_arch = "x86_64", feature = "guest_debug"))]
use crate::api::VmCoredump;
use crate::api::{
    AddDisk, ApiAction, ApiRequest, VmAddDevice, VmAddFs, VmAddNet, VmAddPmem, VmAddUserDevice,
    VmAddVdpa, VmAddVsock, VmBoot, VmConfig, VmCounters, VmDelete, VmNmi, VmPause, VmPowerButton,
    VmReboot, VmReceiveMigration, VmRemoveDevice, VmResize, VmResizeZone, VmRestore, VmResume,
    VmSendMigration, VmShutdown, VmSnapshot,
};
use crate::config::{NetConfig, RestoreConfig};
use micro_http::{Body, Method, Request, Response, StatusCode, Version};
use std::fs::File;
use std::os::unix::io::IntoRawFd;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use vmm_sys_util::eventfd::EventFd;

// /api/v1/vm.create handler
pub struct VmCreate {}

impl EndpointHandler for VmCreate {
    fn handle_request(
        &self,
        req: &Request,
        api_notifier: EventFd,
        api_sender: Sender<ApiRequest>,
    ) -> Response {
        match req.method() {
            Method::Put => {
                match &req.body {
                    Some(body) => {
                        // Deserialize into a VmConfig
                        let mut vm_config: VmConfig = match serde_json::from_slice(body.raw())
                            .map_err(HttpError::SerdeJsonDeserialize)
                        {
                            Ok(config) => config,
                            Err(e) => return error_response(e, StatusCode::BadRequest),
                        };

                        if let Some(ref mut nets) = vm_config.net {
                            if nets.iter().any(|net| net.fds.is_some()) {
                                warn!("Ignoring FDs sent via the HTTP request body");
                            }
                            for net in nets {
                                net.fds = None;
                            }
                        }

                        match crate::api::VmCreate
                            .send(api_notifier, api_sender, Arc::new(Mutex::new(vm_config)))
                            .map_err(HttpError::ApiError)
                        {
                            Ok(_) => Response::new(Version::Http11, StatusCode::NoContent),
                            Err(e) => error_response(e, StatusCode::InternalServerError),
                        }
                    }

                    None => Response::new(Version::Http11, StatusCode::BadRequest),
                }
            }

            _ => error_response(HttpError::BadRequest, StatusCode::BadRequest),
        }
    }
}

pub trait GetHandler {
    fn handle_request(
        &'static self,
        _api_notifier: EventFd,
        _api_sender: Sender<ApiRequest>,
    ) -> std::result::Result<Option<Body>, HttpError> {
        Err(HttpError::BadRequest)
    }
}

pub trait PutHandler {
    fn handle_request(
        &'static self,
        _api_notifier: EventFd,
        _api_sender: Sender<ApiRequest>,
        _body: &Option<Body>,
        _files: Vec<File>,
    ) -> std::result::Result<Option<Body>, HttpError> {
        Err(HttpError::BadRequest)
    }
}

pub trait HttpVmAction: GetHandler + PutHandler + Sync {}

impl<T: GetHandler + PutHandler + Sync> HttpVmAction for T {}

macro_rules! vm_action_get_handler {
    ($action:ty) => {
        impl GetHandler for $action {
            fn handle_request(
                &'static self,
                api_notifier: EventFd,
                api_sender: Sender<ApiRequest>,
            ) -> std::result::Result<Option<Body>, HttpError> {
                self.send(api_notifier, api_sender, ())
                    .map_err(HttpError::ApiError)
            }
        }

        impl PutHandler for $action {}
    };
}

macro_rules! vm_action_put_handler {
    ($action:ty) => {
        impl PutHandler for $action {
            fn handle_request(
                &'static self,
                api_notifier: EventFd,
                api_sender: Sender<ApiRequest>,
                body: &Option<Body>,
                _files: Vec<File>,
            ) -> std::result::Result<Option<Body>, HttpError> {
                if body.is_some() {
                    Err(HttpError::BadRequest)
                } else {
                    self.send(api_notifier, api_sender, ())
                        .map_err(HttpError::ApiError)
                }
            }
        }

        impl GetHandler for $action {}
    };
}

macro_rules! vm_action_put_handler_body {
    ($action:ty) => {
        impl PutHandler for $action {
            fn handle_request(
                &'static self,
                api_notifier: EventFd,
                api_sender: Sender<ApiRequest>,
                body: &Option<Body>,
                _files: Vec<File>,
            ) -> std::result::Result<Option<Body>, HttpError> {
                if let Some(body) = body {
                    self.send(
                        api_notifier,
                        api_sender,
                        serde_json::from_slice(body.raw())?,
                    )
                    .map_err(HttpError::ApiError)
                } else {
                    Err(HttpError::BadRequest)
                }
            }
        }

        impl GetHandler for $action {}
    };
}

macro_rules! vm_action_put_handler_body_with_fds {
    ($action:ty, $config:ty, $fds_field:ident) => {
        impl PutHandler for $action {
            fn handle_request(
                &'static self,
                api_notifier: EventFd,
                api_sender: Sender<ApiRequest>,
                body: &Option<Body>,
                mut files: Vec<File>,
            ) -> std::result::Result<Option<Body>, HttpError> {
                if let Some(body) = body {
                    let mut cfg: $config = serde_json::from_slice(body.raw())?;
                    if cfg.$fds_field.is_some() {
                        warn!("Ignoring FDs sent via the HTTP request body");
                        cfg.$fds_field = None;
                    }
                    if !files.is_empty() {
                        let fds = files.drain(..).map(|f| f.into_raw_fd()).collect();
                        cfg.$fds_field = Some(fds);
                    }
                    self.send(api_notifier, api_sender, cfg)
                        .map_err(HttpError::ApiError)
                } else {
                    Err(HttpError::BadRequest)
                }
            }
        }

        impl GetHandler for $action {}
    };
}

vm_action_get_handler!(VmCounters);

vm_action_put_handler!(VmBoot);
vm_action_put_handler!(VmDelete);
vm_action_put_handler!(VmShutdown);
vm_action_put_handler!(VmReboot);
vm_action_put_handler!(VmPause);
vm_action_put_handler!(VmResume);
vm_action_put_handler!(VmPowerButton);
vm_action_put_handler!(VmNmi);

vm_action_put_handler_body!(VmAddDevice);
vm_action_put_handler_body!(AddDisk);
vm_action_put_handler_body!(VmAddFs);
vm_action_put_handler_body!(VmAddPmem);
vm_action_put_handler_body!(VmAddVdpa);
vm_action_put_handler_body!(VmAddVsock);
vm_action_put_handler_body!(VmAddUserDevice);
vm_action_put_handler_body!(VmRemoveDevice);
vm_action_put_handler_body!(VmResize);
vm_action_put_handler_body!(VmResizeZone);
vm_action_put_handler_body!(VmSnapshot);
vm_action_put_handler_body!(VmReceiveMigration);
vm_action_put_handler_body!(VmSendMigration);

#[cfg(all(target_arch = "x86_64", feature = "guest_debug"))]
vm_action_put_handler_body!(VmCoredump);

vm_action_put_handler_body_with_fds!(VmAddNet, NetConfig, fds);
vm_action_put_handler_body_with_fds!(VmRestore, RestoreConfig, net_fds);

// Common handler for boot, shutdown and reboot
pub struct VmActionHandler {
    action: &'static dyn HttpVmAction,
}

impl VmActionHandler {
    pub fn new(action: &'static dyn HttpVmAction) -> Self {
        VmActionHandler { action }
    }
}

impl EndpointHandler for VmActionHandler {
    fn put_handler(
        &self,
        api_notifier: EventFd,
        api_sender: Sender<ApiRequest>,
        body: &Option<Body>,
        files: Vec<File>,
    ) -> std::result::Result<Option<Body>, HttpError> {
        PutHandler::handle_request(self.action, api_notifier, api_sender, body, files)
    }

    fn get_handler(
        &self,
        api_notifier: EventFd,
        api_sender: Sender<ApiRequest>,
        _body: &Option<Body>,
    ) -> std::result::Result<Option<Body>, HttpError> {
        GetHandler::handle_request(self.action, api_notifier, api_sender)
    }
}

// /api/v1/vm.info handler
pub struct VmInfo {}

impl EndpointHandler for VmInfo {
    fn handle_request(
        &self,
        req: &Request,
        api_notifier: EventFd,
        api_sender: Sender<ApiRequest>,
    ) -> Response {
        match req.method() {
            Method::Get => match crate::api::VmInfo
                .send(api_notifier, api_sender, ())
                .map_err(HttpError::ApiError)
            {
                Ok(info) => {
                    let mut response = Response::new(Version::Http11, StatusCode::OK);
                    let info_serialized = serde_json::to_string(&info).unwrap();

                    response.set_body(Body::new(info_serialized));
                    response
                }
                Err(e) => error_response(e, StatusCode::InternalServerError),
            },
            _ => error_response(HttpError::BadRequest, StatusCode::BadRequest),
        }
    }
}

// /api/v1/vmm.info handler
pub struct VmmPing {}

impl EndpointHandler for VmmPing {
    fn handle_request(
        &self,
        req: &Request,
        api_notifier: EventFd,
        api_sender: Sender<ApiRequest>,
    ) -> Response {
        match req.method() {
            Method::Get => match crate::api::VmmPing
                .send(api_notifier, api_sender, ())
                .map_err(HttpError::ApiError)
            {
                Ok(pong) => {
                    let mut response = Response::new(Version::Http11, StatusCode::OK);
                    let info_serialized = serde_json::to_string(&pong).unwrap();

                    response.set_body(Body::new(info_serialized));
                    response
                }
                Err(e) => error_response(e, StatusCode::InternalServerError),
            },

            _ => error_response(HttpError::BadRequest, StatusCode::BadRequest),
        }
    }
}

// /api/v1/vmm.shutdown handler
pub struct VmmShutdown {}

impl EndpointHandler for VmmShutdown {
    fn handle_request(
        &self,
        req: &Request,
        api_notifier: EventFd,
        api_sender: Sender<ApiRequest>,
    ) -> Response {
        match req.method() {
            Method::Put => {
                match crate::api::VmmShutdown
                    .send(api_notifier, api_sender, ())
                    .map_err(HttpError::ApiError)
                {
                    Ok(_) => Response::new(Version::Http11, StatusCode::OK),
                    Err(e) => error_response(e, StatusCode::InternalServerError),
                }
            }
            _ => error_response(HttpError::BadRequest, StatusCode::BadRequest),
        }
    }
}
