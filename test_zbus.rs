use zbus::{dbus_interface, Connection};

struct Dummy;

#[dbus_interface(name = "org.dummy.Dummy")]
impl Dummy {
    async fn dummy_method(&self, #[zbus(header)] header: zbus::MessageHeader<'_>) -> zbus::fdo::Result<()> {
        Ok(())
    }
}

fn main() {}
