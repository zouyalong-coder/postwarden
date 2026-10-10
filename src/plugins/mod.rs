//! Build-time registration of concrete plugins.
//!
//! Add new implementations here without changing the plugin API or host runtime.

mod example_policy;
mod mail_template;

use std::sync::Arc;

use postwarden_plugin_api::Plugin;

use crate::database::Database;

pub(crate) fn registered_plugins(database: Database) -> Vec<Arc<dyn Plugin>> {
    vec![
        Arc::new(example_policy::ExamplePlugin),
        Arc::new(mail_template::TemplatePlugin::new(database)),
    ]
}
