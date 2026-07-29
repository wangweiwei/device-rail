use async_trait::async_trait;
use devicerail_core::ExecutionControl;

use crate::{DesktopAction, DesktopCapture, DesktopProbe, DesktopProfile, DesktopResult};

/// Injectable native boundary used by all three public desktop Drivers.
///
/// `profile` is immutable and drives the synchronous protection lookup. The
/// asynchronous probe may refresh permissions and viewport, but changing the
/// advertised platform/action contract is rejected by the Driver.
#[async_trait]
pub trait DesktopBackend: Send + Sync {
    fn profile(&self) -> &DesktopProfile;

    async fn probe(&self, control: &ExecutionControl) -> DesktopResult<DesktopProbe>;

    async fn capture(&self, control: &ExecutionControl) -> DesktopResult<DesktopCapture>;

    async fn execute(&self, action: DesktopAction, control: &ExecutionControl)
    -> DesktopResult<()>;
}
