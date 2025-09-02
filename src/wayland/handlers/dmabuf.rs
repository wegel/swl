// SPDX-License-Identifier: GPL-3.0-only

use crate::state::{BackendData, State};
use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    delegate_dmabuf,
    wayland::{
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
    },
};
use tracing::debug;

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        global: &DmabufGlobal,
        dmabuf: Dmabuf,
        import_notifier: ImportNotifier,
    ) {
        
        match &mut self.backend {
            BackendData::Kms(kms) => {
                match kms.dmabuf_imported(global, dmabuf) {
                    Ok(node) => {
                        debug!("Dmabuf imported successfully on node {:?}", node);
                        // Notify client of successful import
                        let _ = import_notifier.successful::<State>();
                    }
                    Err(err) => {
                        debug!("Dmabuf import failed: {:?}", err);
                        import_notifier.failed();
                    }
                }
            }
            BackendData::Uninitialized => {
                debug!("Backend not initialized, failing dmabuf import");
                import_notifier.failed();
            }
        }
    }
}

// BufferHandler is already implemented in wayland/mod.rs

delegate_dmabuf!(State);