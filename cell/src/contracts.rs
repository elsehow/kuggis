//! TypeScript exports for the Kuggis-owned HTTP wire contracts.
//! Generated from the same Serde types that encode the Rust responses.

#[cfg(test)]
mod tests {
    use ts_rs::{Config, TS};

    #[test]
    fn generate_types() {
        let config = Config::from_env();
        crate::operator::resources::ResourcesView::export_all(&config).unwrap();
        crate::operator::resources::ActionView::export_all(&config).unwrap();
        crate::resource::ResourcePlan::export_all(&config).unwrap();
        crate::resource::ResourceOperation::export_all(&config).unwrap();
        crate::publish::registry::resources::ResourceInventory::export_all(&config).unwrap();
        crate::publish::registry::resources::CleanupPlan::export_all(&config).unwrap();
        crate::publish::registry::resources::CleanupReceipt::export_all(&config).unwrap();
        crate::publish::PublicationReceipt::export_all(&config).unwrap();
        crate::chat::oauth::AuthFlow::export_all(&config).unwrap();
        crate::chat::oauth::LoginRequest::export_all(&config).unwrap();
        crate::chat::oauth::AuthReply::export_all(&config).unwrap();
        crate::operator::CellsView::export_all(&config).unwrap();
        crate::operator::creation::Creation::export_all(&config).unwrap();
        crate::operator::deletion::DeletionView::export_all(&config).unwrap();
        crate::chat::settings::SettingsView::export_all(&config).unwrap();
        crate::connection::SecretUpdate::export_all(&config).unwrap();
        crate::connection::ConnectionUpdate::export_all(&config).unwrap();
        crate::operator::Preview::export_all(&config).unwrap();
    }
}
