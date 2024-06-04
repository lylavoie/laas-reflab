// Copyright (c) 2023 University of New Hampshire
// SPDX-License-Identifier: MIT
#![doc = include_str!("../../README.md")]
//! # LICENSE
//!
//! This software is licensed under the MIT license.
//! Copyright (c) 2023 University of New Hampshire
use client::remote::{cli_client_entry, cli_server_entry};
use common::prelude::{
    axum::{self, Json},
    chrono::{self, Days, Utc},
    itertools::Itertools,
    tokio,
    tokio::{sync::mpsc, task::LocalSet},
    tracing,
};
use config::settings;
use core::panic;
use liblaas::{
    self,
    web::{api::TemplateBlob, template::make_template},
};
use models::{
    allocation::AllocationReason,
    dal::{new_client, web::ResultWithCode, AsEasyTransaction, DBTable, FKey, NewRow},
    dashboard::{Aggregate, NetworkAssignmentMap, *},
    inventory::*,
};
use std::{env::args, sync::OnceLock, time::Duration};
use tascii::{self, prelude::*};
use workflows::{self, resource_management::allocator::Allocator};

pub async fn allocate_unreserved_hosts() {
    let dev_hosts = settings().dev.hosts.clone();

    let mut client = new_client().await.log_db_client_error().unwrap();
    tracing::info!("Got client for unreserved hosts");
    let mut transaction = client
        .easy_transaction()
        .await
        .log_db_client_error()
        .unwrap();

    tracing::info!("Got the transaction");
    let now = Utc::now();

    let lab = match Lab::get_by_name(&mut transaction, "reserved".to_string()).await {
        Ok(opt_lab) => match opt_lab {
            Some(l) => l.id,
            None => {
                let mut t = transaction.easy_transaction().await.unwrap();
                let l = NewRow::new(Lab {
                    id: FKey::new_id_dangling(),
                    name: "reserved".to_string(),
                    location: "".to_string(),
                    email: "".to_string(),
                    phone: "".to_string(),
                    is_dynamic: true
                }).insert(&mut t).await.unwrap();
                t.commit().await.unwrap();
                l
            },
        },
        Err(e) => panic!("Failed to find reserved lab, unable to reserve hosts that may be in production until this is fixed, error: {}", e.to_string()),
    };
    println!("Labs: {:?}", Lab::select().run(&mut transaction).await);
    transaction.commit().await.unwrap();

    let mut transaction = client
        .easy_transaction()
        .await
        .log_db_client_error()
        .unwrap();

    let template = make_template(
        axum::extract::path::Path::from(common::prelude::axum::extract::Path(
            "reserved".to_string(),
        )),
        Json(TemplateBlob {
            id: None,
            owner: format!("root"),
            pod_name: format!("reserved"),
            pod_desc: format!("reserved"),
            public: false,
            host_list: vec![],
            networks: vec![],
            lab_name: format!("reserved"),
        }),
    )
    .await
    .expect("couldn't make default allocation template");

    let agg_id = FKey::new_id_dangling();
    let agg = Aggregate {
        lab,
        state: LifeCycleState::Active,
        id: agg_id,
        configuration: AggregateConfiguration {
            ipmi_username: String::new(),
            ipmi_password: String::new(),
        },
        deleted: false,
        users: vec![],
        vlans: NewRow::new(NetworkAssignmentMap::empty())
            .insert(&mut transaction)
            .await
            .unwrap(),
        template: template.0,
        metadata: BookingMetadata {
            booking_id: Some("Dev aggregate".to_owned()),
            owner: Some("Dev".to_string()),
            lab: Some("Dev lab".to_string()),
            purpose: Some("Unallocatted host".to_owned()),
            project: Some("LibLaaS".to_owned()),
            start: Some(now.clone()),
            end: Some(now + Days::new(1000)),
        },
    };
    NewRow::new(agg).insert(&mut transaction).await.unwrap();

    let allocator = Allocator::instance();

    let mut hosts_builder = Host::select();

    for h in dev_hosts {
        hosts_builder = hosts_builder.where_field("server_name").not_equals(h)
    }

    let hosts = hosts_builder.run(&mut transaction).await.unwrap();

    transaction.commit().await.unwrap();

    for host in hosts {
        let mut transaction = client.easy_transaction().await.unwrap();
        println!("Allocating host ({}): ", host.server_name);
        let resp = allocator
            .allocate_specific_host(
                &mut transaction,
                host.id,
                agg_id,
                AllocationReason::ForMaintenance(),
            )
            .await;

        match resp {
            Ok((_vlan, _handle)) => {
                transaction.commit().await.unwrap();
                tracing::info!("Allocated");
            }
            Err(e) => {
                tracing::info!("error getting resource: {e:?}");
                transaction.rollback().await.unwrap();
            }
        }
    }
}

pub fn clear_tasks() {
    std::fs::remove_file("primary-targets.json").ok();
}

/// 1. start DB with models, do migration hooks
/// 2. start TASCII runtime
/// 3. give runtime ref to web
/// 4. start web
#[tokio::main]
async fn main() {
    let args = args().collect_vec();
    match args.get(1).map(|s| s.as_str()) {
        Some("--cli") => {
            println!("Starting in CLI Mode");
            cli_client_entry().await;
            return;
        }
        Some("--server") => {
            println!("Starting in Server Mode");
        }
        Some(other) => {
            panic!("Unknown CLI option: {other}");
        }
        _ => {
            println!(
                "WARN: zero-arg invocation of LibLaaS will be deprecated soon, pending CLI parsing"
            );
            println!("Defaulting to Starting in Server Mode");
        }
    };

    let subscriber = tracing_subscriber::fmt::fmt().pretty();

    let subscriber = subscriber.with_max_level(config::settings().logging.max_level);

    if let Some(output_file) = config::settings().logging.log_file.clone() {
        let file = std::fs::File::create(&output_file).expect("couldn't open log file");
        let file = std::sync::Mutex::new(file);

        let subscriber = subscriber.with_writer(file).finish();

        let _ =
            tracing::subscriber::set_global_default(subscriber).expect("couldn't set up tracing");
    } else {
        let subscriber = subscriber.finish();

        let _ =
            tracing::subscriber::set_global_default(subscriber).expect("couldn't set up tracing");
    };

    tracing::info!("tracing has been started");
    tracing::debug!("debug tracing has been started");

    clear_tasks();

    unsafe { backtrace_on_stack_overflow::enable() };

    // Run migrations
    let ih = tokio::spawn(async {
        match models::dal::initialize().await {
            Ok(_) => {}
            Err(e) => {
                for error in e {
                    tracing::error!("Init Error: {}, check logs for panic", error.to_string())
                }
            }
        }
    });

    let _ = ih.await;

    // Reserve all hosts that are not dev hosts if dev mode is on
    let dev = settings().dev.clone();
    if dev.status {
        tracing::info!("Running LibLaaS as dev.");
        tracing::info!("Clearing all tasks.");
        tracing::info!("Allocating unreserved hosts");
        allocate_unreserved_hosts().await;
    } else {
        tracing::info!("Running LibLaaS as prod");
    }

    tracing::info!("starting tascii runtime");
    let tascii_rt = start_tascii();

    tracing::info!("Sets up dispatcher");
    workflows::entry::Dispatcher::init(tascii_rt); // make sure we have something to push to

    let wh = tokio::spawn(async {
        tracing::info!("starting web");
        tracing::info!("Runs web entry");
        let v = liblaas::web::entry(tascii_rt).await;
        tracing::info!("web exited");
        v
    });

    let mh = tokio::spawn(async {
        tracing::info!("starting mailbox");
        let v = workflows::resource_management::mailbox::entry(tascii_rt).await;
        tracing::info!("mailbox exited");
        v
    });

    std::thread::sleep(Duration::from_secs(1));

    let l = LocalSet::new();

    l.spawn_local(async { mh.await });
    l.spawn_local(async { wh.await });

    let (liblaas_tx, mut liblaas_rx) = mpsc::channel(5);

    l.spawn_local(async move {
        loop {
            let liblaas_tx = liblaas_tx.clone();
            let res = cli_server_entry(tascii_rt, liblaas_tx).await;
        }
    });

    l.run_until(async move {
        loop {
            let msg = liblaas_rx.recv().await;
            match msg {
                Some(client::LiblaasStateInstruction::ShutDown) => break,
                Some(client::LiblaasStateInstruction::ExitCLI) => {
                    tracing::info!("Client exited CLI cleanly");
                    continue;
                }
                Some(client::LiblaasStateInstruction::DoNothing) => {
                    tracing::info!("NOOP CLI msg");
                    continue;
                }
                None => {
                    tracing::error!("CLI msg channel empty?");
                    continue;
                }
            }
        }
    })
    .await;

    std::mem::drop(l);

    tracing::info!("Clean exit from web entry");
}

const TASCII_RT: OnceLock<&'static Runtime> = OnceLock::new();

/// returns immediately
fn start_tascii() -> &'static Runtime {
    let runtime = tascii::init("primary");

    let _ = TASCII_RT.set(runtime);

    runtime
}
