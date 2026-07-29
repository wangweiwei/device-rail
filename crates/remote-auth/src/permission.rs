use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    Read,
    Control,
    Admin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    id: String,
    permissions: BTreeSet<Permission>,
}

impl AuthenticatedPrincipal {
    pub(crate) fn new(id: String, permissions: BTreeSet<Permission>) -> Self {
        Self { id, permissions }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn permissions(&self) -> &BTreeSet<Permission> {
        &self.permissions
    }

    pub fn allows(&self, required: Permission) -> bool {
        self.permissions.iter().any(|granted| match granted {
            Permission::Admin => true,
            Permission::Control => required != Permission::Admin,
            Permission::Read => required == Permission::Read,
        })
    }
}

/// Maps every public application RPC method to a minimum permission. Auth
/// prelude methods are intentionally absent because they are handled before
/// application dispatch. Unknown methods return `None` and must be denied by
/// the caller rather than inheriting a default permission.
pub fn required_permission(method: &str) -> Option<Permission> {
    match method {
        "system.hello"
        | "system.describe"
        | "devices.list"
        | "device.capabilities"
        | "session.current"
        | "sessions.list"
        | "session.export"
        | "events.list"
        | "events.stream.open"
        | "ui.snapshot.get" => Some(Permission::Read),
        "device.select"
        | "device.connect"
        | "device.disconnect"
        | "device.observe"
        | "device.execute"
        | "media.stream.start"
        | "media.stream.capture"
        | "media.stream.end"
        | "request.cancel"
        | "session.start"
        | "session.end"
        | "verdict.record" => Some(Permission::Control),
        "events.clear" => Some(Permission::Admin),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{AuthenticatedPrincipal, Permission, required_permission};

    #[test]
    fn permissions_are_hierarchical_and_every_known_method_is_mapped() {
        let read = AuthenticatedPrincipal::new("reader".into(), BTreeSet::from([Permission::Read]));
        let control =
            AuthenticatedPrincipal::new("controller".into(), BTreeSet::from([Permission::Control]));
        let admin =
            AuthenticatedPrincipal::new("admin".into(), BTreeSet::from([Permission::Admin]));
        assert!(read.allows(Permission::Read));
        assert!(!read.allows(Permission::Control));
        assert!(control.allows(Permission::Read));
        assert!(control.allows(Permission::Control));
        assert!(!control.allows(Permission::Admin));
        assert!(admin.allows(Permission::Admin));

        for method in [
            "system.hello",
            "system.describe",
            "devices.list",
            "device.select",
            "device.connect",
            "device.disconnect",
            "device.capabilities",
            "device.observe",
            "device.execute",
            "media.stream.start",
            "media.stream.capture",
            "media.stream.end",
            "request.cancel",
            "session.start",
            "session.current",
            "session.end",
            "sessions.list",
            "session.export",
            "events.list",
            "events.clear",
            "events.stream.open",
            "ui.snapshot.get",
            "verdict.record",
        ] {
            assert!(required_permission(method).is_some(), "unmapped {method}");
        }
        assert_eq!(required_permission("future.method"), None);
    }
}
