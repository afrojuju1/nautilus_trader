//! Normalized order-leg plans for Alpaca options submissions.

use nautilus_model::enums::OrderSide;

/// One option leg in a planned submit request.
#[derive(Clone, Debug)]
pub struct OrderLegSpec {
    /// Stable client-order ID suffix.
    pub label: String,
    /// Alpaca option symbol.
    pub symbol: String,
    /// Buy or sell side.
    pub side: OrderSide,
    /// Contract quantity.
    pub quantity: u64,
    /// Limit price.
    pub limit_price: f64,
    /// Whether this leg reduces existing exposure.
    pub reduce_only: bool,
}

impl OrderLegSpec {
    /// Builds a new order-leg spec.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        symbol: impl Into<String>,
        side: OrderSide,
        quantity: u64,
        limit_price: f64,
        reduce_only: bool,
    ) -> Self {
        Self {
            label: label.into(),
            symbol: symbol.into(),
            side,
            quantity,
            limit_price,
            reduce_only,
        }
    }
}

/// Planned single-leg or multi-leg options submission.
#[derive(Clone, Debug)]
pub struct SubmitPlan {
    /// Parent order-list or single client-order ID.
    pub client_order_id: String,
    /// Planned option legs.
    pub legs: Vec<OrderLegSpec>,
}

impl SubmitPlan {
    /// Builds a submit plan.
    ///
    /// # Panics
    ///
    /// Panics if no legs are supplied. Callers should validate strategy-specific quantities before
    /// constructing the plan.
    #[must_use]
    pub fn new(client_order_id: impl Into<String>, legs: Vec<OrderLegSpec>) -> Self {
        assert!(
            !legs.is_empty(),
            "submit plans must include at least one leg"
        );
        Self {
            client_order_id: client_order_id.into(),
            legs,
        }
    }

    /// Number of expected execution events for accepted/rejected legs.
    #[must_use]
    pub fn expected_events(&self) -> usize {
        self.legs.len()
    }

    /// Returns true for a single-leg order.
    #[must_use]
    pub fn is_single_leg(&self) -> bool {
        self.legs.len() == 1
    }
}
