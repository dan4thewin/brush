use std::{
    borrow::{Borrow, BorrowMut},
    sync::Arc,
};

use tokio::sync::{Mutex, MutexGuard};

pub(crate) type ShellRef = Arc<Mutex<brush_core::Shell>>;

pub(crate) struct ReedlineShellReader<'a> {
    pub shell: MutexGuard<'a, brush_core::Shell>,
}

impl AsRef<brush_core::Shell> for ReedlineShellReader<'_> {
    fn as_ref(&self) -> &brush_core::Shell {
        self.shell.borrow()
    }
}

pub(crate) struct ReedlineShellWriter<'a> {
    pub shell: MutexGuard<'a, brush_core::Shell>,
}

impl AsMut<brush_core::Shell> for ReedlineShellWriter<'_> {
    fn as_mut(&mut self) -> &mut brush_core::Shell {
        self.shell.borrow_mut()
    }
}
