//! Gold coins page: add, list and delete coins entered by hand.

use std::str::FromStr;

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{Datelike, NaiveDate};
use portfolio_core::gold::TENTH_OUNCE_GRAMS;
use portfolio_db::gold::{self as store, Coin, NewCoin};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::base::{Page, Shell};
use crate::{AppError, AppState};

/// Raw form input, kept as typed so it can be shown back with errors.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct CoinForm {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub purchased_on: String,
    #[serde(default)]
    pub purchase_price: String,
    #[serde(default)]
    pub fine_weight: String,
    #[serde(default)]
    pub manual_quote: String,
}

impl CoinForm {
    fn empty() -> Self {
        CoinForm {
            fine_weight: TENTH_OUNCE_GRAMS.to_string(),
            ..CoinForm::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldErrors {
    pub label: Option<String>,
    pub purchased_on: Option<String>,
    pub purchase_price: Option<String>,
    pub fine_weight: Option<String>,
    pub manual_quote: Option<String>,
}

/// Parses an amount typed by hand: spaces (also non-breaking) are ignored and a decimal comma
/// is accepted, so "1 234,56" and "1234.56" are the same. Must be positive, with at most
/// `max_decimals` decimal places.
fn parse_amount(raw: &str, max_decimals: u32) -> Result<Decimal, String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}')
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    if cleaned.is_empty() {
        return Err("required".into());
    }
    if cleaned.starts_with('-') {
        return Err("must be more than 0".into());
    }
    if !cleaned.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err("enter a number like 1234.56".into());
    }
    let value =
        Decimal::from_str(&cleaned).map_err(|_| "enter a number like 1234.56".to_owned())?;
    if value <= Decimal::ZERO {
        return Err("must be more than 0".into());
    }
    if value.scale() > max_decimals {
        return Err(format!("at most {max_decimals} decimal places"));
    }
    Ok(value)
}

/// Validates the form. `today` bounds the purchase date (no future purchases).
pub fn parse_form(form: &CoinForm, today: NaiveDate) -> Result<NewCoin, FieldErrors> {
    let mut errors = FieldErrors::default();

    let label = form.label.trim();
    if label.chars().count() > 100 {
        errors.label = Some("at most 100 characters".into());
    }
    let purchased_on = match NaiveDate::from_str(form.purchased_on.trim()) {
        Err(_) => {
            errors.purchased_on = Some("enter a date (YYYY-MM-DD)".into());
            None
        }
        Ok(d) if d > today => {
            errors.purchased_on = Some("cannot be in the future".into());
            None
        }
        Ok(d) if d.year() < 1900 => {
            errors.purchased_on = Some("too far in the past".into());
            None
        }
        Ok(d) => Some(d),
    };
    let price = parse_amount(&form.purchase_price, 2)
        .map_err(|e| errors.purchase_price = Some(e))
        .ok();
    let weight = parse_amount(&form.fine_weight, 4)
        .map_err(|e| errors.fine_weight = Some(e))
        .ok();
    let quote = if form.manual_quote.trim().is_empty() {
        Some(None)
    } else {
        parse_amount(&form.manual_quote, 2)
            .map(Some)
            .map_err(|e| errors.manual_quote = Some(e))
            .ok()
    };

    match (purchased_on, price, weight, quote) {
        (Some(purchased_on), Some(purchase_price_pln), Some(fine_weight_g), Some(manual))
            if errors == FieldErrors::default() =>
        {
            Ok(NewCoin {
                label: (!label.is_empty()).then(|| label.to_owned()),
                purchased_on,
                purchase_price_pln,
                fine_weight_g,
                manual_quote_pln: manual,
            })
        }
        _ => Err(errors),
    }
}

/// A coin formatted for display.
pub struct CoinRow {
    pub id: i64,
    pub purchased_on: String,
    pub label: String,
    pub purchase_price: String,
    pub fine_weight: String,
    pub manual_quote: String,
}

fn money(value: Decimal) -> String {
    format!("{:.2}", value)
}

impl From<&Coin> for CoinRow {
    fn from(c: &Coin) -> Self {
        CoinRow {
            id: c.id,
            purchased_on: c.purchased_on.to_string(),
            label: c.label.clone().unwrap_or_default(),
            purchase_price: money(c.purchase_price_pln),
            fine_weight: c.fine_weight_g.normalize().to_string(),
            manual_quote: c.manual_quote_pln.map(money).unwrap_or_default(),
        }
    }
}

#[derive(Template)]
#[template(path = "gold_panel.html")]
pub struct GoldPanel {
    pub coins: Vec<CoinRow>,
    pub total_price: String,
    pub total_weight: String,
    pub form: CoinForm,
    pub errors: FieldErrors,
}

#[derive(Template)]
#[template(path = "gold.html")]
pub struct GoldPage {
    pub shell: Shell,
    pub coins: Vec<CoinRow>,
    pub total_price: String,
    pub total_weight: String,
    pub form: CoinForm,
    pub errors: FieldErrors,
}

fn panel(coins: &[Coin], form: CoinForm, errors: FieldErrors) -> GoldPanel {
    let total_price: Decimal = coins.iter().map(|c| c.purchase_price_pln).sum();
    let total_weight: Decimal = coins.iter().map(|c| c.fine_weight_g).sum();
    GoldPanel {
        coins: coins.iter().map(CoinRow::from).collect(),
        total_price: money(total_price),
        total_weight: total_weight.normalize().to_string(),
        form,
        errors,
    }
}

fn page(p: GoldPanel, shell: Shell) -> GoldPage {
    GoldPage {
        shell,
        coins: p.coins,
        total_price: p.total_price,
        total_weight: p.total_weight,
        form: p.form,
        errors: p.errors,
    }
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v == "true")
}

fn render(template: &impl Template) -> Result<Html<String>, AppError> {
    Ok(Html(template.render()?))
}

pub async fn show(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, AppError> {
    let coins = store::list_coins(&state.pool).await?;
    let shell = Shell::load(&state, &headers, Page::Gold).await?;
    Ok(render(&page(
        panel(&coins, CoinForm::empty(), FieldErrors::default()),
        shell,
    ))?
    .into_response())
}

/// htmx requests get the refreshed panel; plain form posts are redirected back to the page
/// (or get the page with errors and 422).
pub async fn add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CoinForm>,
) -> Result<Response, AppError> {
    let result = parse_form(&form, (state.today)());
    if let Ok(coin) = &result {
        store::add_coin(&state.pool, coin).await?;
    }
    let coins = store::list_coins(&state.pool).await?;
    match (result, is_htmx(&headers)) {
        (Ok(_), true) => {
            Ok(render(&panel(&coins, CoinForm::empty(), FieldErrors::default()))?.into_response())
        }
        (Ok(_), false) => Ok(Redirect::to("/gold").into_response()),
        // htmx only swaps 2xx responses, so validation errors are a 200 panel there.
        (Err(errors), true) => Ok(render(&panel(&coins, form, errors))?.into_response()),
        (Err(errors), false) => {
            let shell = Shell::load(&state, &headers, Page::Gold).await?;
            Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                render(&page(panel(&coins, form, errors), shell))?,
            )
                .into_response())
        }
    }
}

pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if !store::delete_coin(&state.pool, id).await? {
        return Ok((StatusCode::NOT_FOUND, "coin not found").into_response());
    }
    if !is_htmx(&headers) {
        return Ok(Redirect::to("/gold").into_response());
    }
    let coins = store::list_coins(&state.pool).await?;
    Ok(render(&panel(&coins, CoinForm::empty(), FieldErrors::default()))?.into_response())
}
