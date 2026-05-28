//! When the request body omits `model`, pick a default from policy + pricing
//! and inject into the body. Policy names align with DB `policy_configs.name`
//! (the `Model Allowlist` row).

/// `policy_configs` row for Model Allowlist: `name` matches DB (product
/// naming).
pub const POLICY_NAME_MODEL_ALLOWLIST: &str = "Model Allowlist";

mod choose;
mod price;
mod resolve;

pub use choose::{choose_default_gateway_model, choose_default_gateway_model_excluding_provider};
pub use price::price_sum_from_info;
pub use resolve::{
    first_non_empty_list, model_ids_from_config_json, model_ids_from_overrides_json,
    model_ids_from_policy_overrides_json, pick_greatest_by_price_and_name,
};

#[cfg(test)]
mod policy_name_tests {
    #[test]
    fn policy_config_name_is_model_allowlist() {
        assert_eq!(
            super::POLICY_NAME_MODEL_ALLOWLIST,
            "Model Allowlist",
            "must match policy_configs.name in DB / product"
        );
    }
}
