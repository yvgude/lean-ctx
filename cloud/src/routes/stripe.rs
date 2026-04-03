use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use crate::auth;
use crate::db::{queries, DbPool};

#[derive(Deserialize)]
pub struct CheckoutRequest {
    pub plan: String,
}

#[derive(Serialize)]
pub struct CheckoutResponse {
    pub checkout_url: String,
}

pub async fn create_checkout(
    State(db): State<DbPool>,
    req: axum::extract::Request,
) -> Result<Json<CheckoutResponse>, StatusCode> {
    let api_key = auth::extract_api_key(&req).ok_or(StatusCode::UNAUTHORIZED)?;
    let _user = auth::authenticate(&db, &api_key).ok_or(StatusCode::UNAUTHORIZED)?;

    let body = axum::body::to_bytes(req.into_body(), 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let payload: CheckoutRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    let stripe_key = std::env::var("STRIPE_SECRET_KEY")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let price_id = match payload.plan.as_str() {
        "pro" => std::env::var("STRIPE_PRO_PRICE_ID").map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        "team" => std::env::var("STRIPE_TEAM_PRICE_ID").map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let form_body = format!(
        "mode=subscription&line_items[0][price]={price_id}&line_items[0][quantity]=1&success_url={}/dashboard&cancel_url={}/pricing",
        base_url(), base_url()
    );

    let resp = ureq::post("https://api.stripe.com/v1/checkout/sessions")
        .header("Authorization", &format!("Bearer {stripe_key}"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(form_body.as_bytes())
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let resp_body = resp.into_body().read_to_string().map_err(|_| StatusCode::BAD_GATEWAY)?;
    let body: serde_json::Value = serde_json::from_str(&resp_body).map_err(|_| StatusCode::BAD_GATEWAY)?;
    let url = body["url"].as_str().unwrap_or("").to_string();

    Ok(Json(CheckoutResponse { checkout_url: url }))
}

pub async fn webhook(
    State(db): State<DbPool>,
    body: axum::body::Bytes,
) -> StatusCode {
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    let event_type = payload["type"].as_str().unwrap_or("");

    match event_type {
        "checkout.session.completed" => {
            let customer_email = payload["data"]["object"]["customer_details"]["email"]
                .as_str()
                .unwrap_or("");
            let subscription_id = payload["data"]["object"]["subscription"]
                .as_str()
                .unwrap_or("");

            if let Some(user) = queries::find_user_by_email(&db, customer_email) {
                let _ = queries::update_user_plan(&db, &user.id, "pro", Some(subscription_id));
            }
        }
        "customer.subscription.deleted" => {
            let customer_email = payload["data"]["object"]["customer_email"]
                .as_str()
                .unwrap_or("");
            if let Some(user) = queries::find_user_by_email(&db, customer_email) {
                let _ = queries::update_user_plan(&db, &user.id, "free", None);
            }
        }
        _ => {}
    }

    StatusCode::OK
}

fn base_url() -> String {
    std::env::var("BASE_URL").unwrap_or_else(|_| "https://leanctx.com".to_string())
}
