//! A module's manifest, as a host reads it back from JSON.

use pamin_kit::{Manifest, Module, ModuleName};

#[path = "../examples/echo/main.rs"]
mod echo;
use echo::Echo;

#[test]
fn a_manifest_reads_back_and_its_name_is_checked() {
    let manifest = Echo.manifest();
    let printed = serde_json::to_string(&manifest).unwrap();
    let read: Manifest<String, ModuleName> = serde_json::from_str(&printed).unwrap();
    assert_eq!(
        serde_json::to_value(&read).unwrap(),
        serde_json::to_value(&manifest).unwrap()
    );

    for bad in ["", "9memory", "Memory", "my_module", "mémoire"] {
        let named = Manifest {
            name: bad,
            ..manifest.clone()
        };
        let printed = serde_json::to_string(&named).unwrap();
        let read = serde_json::from_str::<Manifest<String, ModuleName>>(&printed);
        assert!(read.is_err(), "{bad:?}");
    }
}
