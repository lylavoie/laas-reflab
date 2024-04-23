//! Copyright (c) 2023 University of New Hampshire
//! SPDX-License-Identifier: MIT

//! This module should probably actually be removed, if liblaas
//! can carry around a ref to the runtime all the time then it can
//! directly spawn in tasks and that simplifies control flow
//!
//! this mod does mean more clear separation and looser coupling,
//! but I don't think it's strictly worth it when the entire project
//! is as small as it is (relatively speaking)

use std::{sync::Mutex};

use models::{
    dal::{new_client, AsEasyTransaction, FKey, ID},
    dashboard::Aggregate,
};

use models::inventory;

use common::prelude::{anyhow, crossbeam_channel, once_cell};

use crossbeam_channel::{Receiver, Sender};
use models::dal::web::*;

use tascii::{prelude::*};

//use crate::actions::{Action, ActionID, StatusHandle};

//use crate::actions::*;

static ACTION_LOG_LOCK: Mutex<()> = Mutex::new(());

pub enum Action {
    DeployBooking { agg_id: FKey<Aggregate> },
    CleanupBooking { agg_id: FKey<Aggregate> },
    // AddUsers { agg_id: LLID, users: dashboard::UserData },
    // UpdateUser { agg_id: LLID, user: dashboard::UserData },
    // RemoveUser { agg_id: LLID, user: i64 },
    // Reimage { agg_id: LLID, data: dashboard::ReimageData },
    // AddInstance { agg_id: LLID, instance: dashboard::InstanceData },
    // RemoveInstance { agg_id: LLID, instance: dashboard::InstanceData },
}

pub struct Dispatcher {
    rt: &'static Runtime,
}

//static ref DISPATCH: Sender<Action>;

pub static DISPATCH: once_cell::sync::OnceCell<Sender<Action>> = once_cell::sync::OnceCell::new();
// DISPATCH.get().unwrap().send(Action::DeployBooking { agg_id: <something> });

impl Dispatcher {
    pub fn init(rt: &'static Runtime) {
        let (s, r) = crossbeam_channel::unbounded();

        let d = Self { rt };

        std::thread::spawn(|| {
            d.handler(r);
        });

        DISPATCH.set(s).expect("dispatcher was already initialized");
    }

    pub fn handler(self, recv: Receiver<Action>) {
        while let Ok(v) = recv.recv() {
            match v {
                Action::DeployBooking { agg_id } => {
                    // first, get all resources that are a member of the booking
                    // create a task using the aggregate and spawn it into the runtime
                    // use set_depends to say that that task can only run once
                    // the hosts have finished what they were doing before
                    // use the passed back task_id from creating the task in the first place to track progress

                    let task = crate::deploy_booking::BookingTask {
                        aggregate_id: agg_id,
                    };

                    let task_id = self.rt.enroll(task.into());
                    self.rt.set_target(task_id);

                }
                Action::CleanupBooking { agg_id } => {
                    let task = crate::cleanup_booking::CleanupAggregate { agg_id };

                    let task_id = self.rt.enroll(task.into());
                    self.rt.set_target(task_id);
                }
                // Action::AddUsers { agg_id, users } => {
                //     // TODO: Create task
                //     let task_id: LLID = self.rt.enroll(todo!());
                //     self.rt.set_target(task_id);
                // },
                // Action::UpdateUser { agg_id, user } => {
                //     // TODO: Create task
                //     let task_id: LLID = self.rt.enroll(todo!());
                //     self.rt.set_target(task_id);
                // },
                // Action::RemoveUser { agg_id, user } => {
                //     // TODO: Create task
                //     let task_id: LLID = self.rt.enroll(todo!());
                //     self.rt.set_target(task_id);
                // },
                // Action::Reimage { agg_id, data } => {
                //     // TODO: Create task
                //     let task_id: LLID = self.rt.enroll(todo!());
                //     self.rt.set_target(task_id);
                // },
                // Action::AddInstance { agg_id, instance } => {
                //     // TODO: Create task
                //     let task_id: LLID = self.rt.enroll(todo!());
                //     self.rt.set_target(task_id);
                // },
                // Action::RemoveInstance { agg_id, instance } => {
                //     // TODO: Create task
                //     let task_id: LLID = self.rt.enroll(todo!());
                //     self.rt.set_target(task_id);
                // },
            }
        }
    }

    async fn set_depends(
        &self,
        id: ID,
        resources: Vec<FKey<inventory::Host>>,
    ) -> Result<(), anyhow::Error> {
        let mut client = new_client().await?;
        let mut transaction = client.easy_transaction().await?;

        /*let options = TransactionOptions::builder()
        .read_concern(ReadConcern::majority())
        .write_concern(WriteConcern::builder().w(Acknowledgment::Majority).build())
        .build();*/

        //let coll = Host::all_hosts(&mut client).unwrap();

        for resource in resources {
            let _ = inventory::Action::add_for_host(&mut transaction, resource, false, id)
                .await
                .log_server_error("couldn't set depends for an action", true);
        }
        transaction.commit().await.unwrap();
        Ok(())
    }
}
