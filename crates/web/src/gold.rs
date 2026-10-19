//! The Gold page (`design/screens/06`): the summary tiles (valuation, paid, P&L, holding), the
//! coins table with each coin's value and P&L, the inline add form with its field errors, and
//! the "Purchases paused" badge when gold is paused in the config.
//!
//! Each coin is valued as everywhere else: NBP price per gram x fine weight x the buyback factor,
//! or its own dealer quote ([`portfolio_core::gold::value_coin`]). Without an NBP price the coins
//! without a quote show "—" and the page still renders, and adding or removing coins works.

use std::str::FromStr;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Extension, Form};
use chrono::{Datelike, NaiveDate};
use portfolio_core::AssetClass;
use portfolio_core::config::Config;
use portfolio_core::gold::{TENTH_OUNCE_GRAMS, ValuationBasis, value_coin};
use portfolio_core::price::PriceQuote;
use portfolio_db::gold::{self as store, Coin, NewCoin};
use portfolio_sources::gold::GOLD_GRAM;
use portfolio_sources::prices::close_on_day;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::base::{Page, Shell};
use crate::format::{self, Delta};
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
    /// A blank form: bought today, the 1/10 oz weight.
    pub fn empty(today: NaiveDate) -> Self {
        CoinForm {
            purchased_on: today.to_string(),
            fine_weight: format::number(TENTH_OUNCE_GRAMS, 2),
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

/// The components sheet's error for an amount that is no number.
const AMOUNT_LIKE: &str = "Enter an amount like 2 050,00";

/// Parses an amount typed by hand: spaces (also non-breaking) are ignored and a decimal comma
/// is accepted, so "1 234,56" and "1234.56" are the same. Must be positive, with at most
/// `max_decimals` decimal places. `not_a_number` is the error for text that is no number.
fn parse_amount(raw: &str, max_decimals: u32, not_a_number: &str) -> Result<Decimal, String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}')
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    if cleaned.is_empty() {
        return Err("Required".into());
    }
    let like = || not_a_number.to_owned();
    if cleaned.starts_with('-') {
        return Err("Must be more than 0".into());
    }
    if !cleaned.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(like());
    }
    let value = Decimal::from_str(&cleaned).map_err(|_| like())?;
    if value <= Decimal::ZERO {
        return Err("Must be more than 0".into());
    }
    if value.scale() > max_decimals {
        return Err(format!("At most {max_decimals} decimal places"));
    }
    Ok(value)
}

/// Validates the form. `today` bounds the purchase date (no future purchases).
pub fn parse_form(form: &CoinForm, today: NaiveDate) -> Result<NewCoin, FieldErrors> {
    let mut errors = FieldErrors::default();

    let label = form.label.trim();
    if label.chars().count() > 100 {
        errors.label = Some("At most 100 characters".into());
    }
    let purchased_on = match NaiveDate::from_str(form.purchased_on.trim()) {
        Err(_) => {
            errors.purchased_on = Some("Enter a date".into());
            None
        }
        Ok(d) if d > today => {
            errors.purchased_on = Some("Cannot be in the future".into());
            None
        }
        Ok(d) if d.year() < 1900 => {
            errors.purchased_on = Some("Too far in the past".into());
            None
        }
        Ok(d) => Some(d),
    };
    let price = parse_amount(&form.purchase_price, 2, AMOUNT_LIKE)
        .map_err(|e| errors.purchase_price = Some(e))
        .ok();
    let weight = parse_amount(&form.fine_weight, 4, "Enter a weight like 3,11")
        .map_err(|e| errors.fine_weight = Some(e))
        .ok();
    let quote = if form.manual_quote.trim().is_empty() {
        Some(None)
    } else {
        parse_amount(&form.manual_quote, 2, AMOUNT_LIKE)
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

/// Grams of fine gold in a troy ounce.
const TROY_OUNCE_GRAMS: Decimal = Decimal::from_parts(311_034_768, 0, 0, false, 7);

/// What a coin is worth now and on what basis: its dealer quote, or the NBP price per gram x
/// weight x `buyback_factor`. None without a quote when there is no NBP price.
pub fn coin_value(
    coin: &Coin,
    price_per_gram: Option<Decimal>,
    buyback_factor: Decimal,
) -> Option<(Decimal, ValuationBasis)> {
    match (coin.manual_quote_pln, price_per_gram) {
        (Some(quote), _) => Some((quote, ValuationBasis::ManualQuote)),
        (None, Some(price)) => value_coin(price, coin.fine_weight_g, buyback_factor, None)
            .ok()
            .map(|v| (v.value_pln, v.basis)),
        (None, None) => None,
    }
}

/// Troy ounces for grams of fine gold, as the page shows them: "0,1" for 3,11 g, "0,05" for half
/// that, "1,0" for 31,1 g (two decimals at most, one at least).
pub fn ounces_text(grams: Decimal) -> String {
    let oz = (grams / TROY_OUNCE_GRAMS).round_dp(2).normalize();
    format::number(oz, oz.scale().max(1))
}

/// Grams as typed in the config or the form: "3,11", "7,776".
fn grams_text(grams: Decimal) -> String {
    let g = grams.normalize();
    format::number(g, g.scale().max(2))
}

/// A coin's P&L: "▲ +460,00 zł" in the gain colour, and "+28,0%" of what was paid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pnl {
    pub delta: Delta,
    pub percent: String,
}

impl Pnl {
    fn new(value: Decimal, paid: Decimal) -> Pnl {
        let gain = value - paid;
        Pnl {
            delta: format::delta_pln(gain),
            percent: if paid.is_zero() {
                "—".into()
            } else {
                format::signed_percent(gain / paid)
            },
        }
    }
}

/// A row of the coins table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinRow {
    pub id: i64,
    /// The label, or "1/10 oz coin" (another weight: "Gold coin").
    pub name: String,
    /// "0,1 oz · 3,11 g fine gold".
    pub weight: String,
    /// "12.06.2025".
    pub bought: String,
    pub paid: String,
    /// "2 100,00 zł", or "—" without a price.
    pub value: String,
    /// Valued at its own dealer quote rather than the NBP price.
    pub quoted: bool,
    pub pnl: Option<Pnl>,
}

impl CoinRow {
    pub fn new(coin: &Coin, value: Option<(Decimal, ValuationBasis)>) -> CoinRow {
        let name = match &coin.label {
            Some(label) => label.clone(),
            None if coin.fine_weight_g == TENTH_OUNCE_GRAMS => "1/10 oz coin".into(),
            None => "Gold coin".into(),
        };
        CoinRow {
            id: coin.id,
            name,
            weight: format!(
                "{}{}oz · {}{}g fine gold",
                ounces_text(coin.fine_weight_g),
                format::NBSP,
                grams_text(coin.fine_weight_g),
                format::NBSP
            ),
            bought: format::date(coin.purchased_on),
            paid: format::pln(coin.purchase_price_pln),
            value: value.map_or("—".into(), |(v, _)| format::pln(v)),
            quoted: value.is_some_and(|(_, b)| b == ValuationBasis::ManualQuote),
            pnl: value.map(|(v, _)| Pnl::new(v, coin.purchase_price_pln)),
        }
    }

    /// The remove button's label: "Remove 1/10 oz coin bought 12.06.2025".
    pub fn remove_label(&self) -> String {
        format!("Remove {} bought {}", self.name, self.bought)
    }
}

/// The four tiles above the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The coins' value, or "—" while any of them has none.
    pub value: String,
    pub paid: String,
    /// None without coins or while any coin has no value.
    pub pnl: Option<Pnl>,
    /// "1 coin · 0,1 oz".
    pub holding: String,
}

impl Summary {
    pub fn new(coins: &[Coin], values: &[Option<(Decimal, ValuationBasis)>]) -> Summary {
        let paid: Decimal = coins.iter().map(|c| c.purchase_price_pln).sum();
        let value: Option<Decimal> = values.iter().map(|v| v.map(|(v, _)| v)).sum();
        let grams: Decimal = coins.iter().map(|c| c.fine_weight_g).sum();
        let count = coins.len();
        Summary {
            value: value.map_or("—".into(), format::pln),
            paid: format::pln(paid),
            pnl: value
                .filter(|_| count > 0)
                .map(|value| Pnl::new(value, paid)),
            holding: format!(
                "{count} {} · {}{}oz",
                if count == 1 { "coin" } else { "coins" },
                ounces_text(grams),
                format::NBSP
            ),
        }
    }
}

/// The header's note on how coins are valued: "Valued at NBP spot × 0,95 · 1 181,80 zł per
/// 1/10 oz · 15.10.2026" (the factor is left out when it is 1).
pub fn valued_at(quote: Option<&PriceQuote>, buyback_factor: Decimal) -> String {
    let Some(q) = quote else {
        return "No NBP gold price right now · coins with a dealer quote are valued at it".into();
    };
    let factor = if buyback_factor == Decimal::ONE {
        String::new()
    } else {
        format!(
            " × {}",
            format::number(buyback_factor, buyback_factor.normalize().scale().max(2))
        )
    };
    format!(
        "Valued at NBP spot{factor} · {} per 1/10{}oz · {}",
        format::pln(q.close * TENTH_OUNCE_GRAMS * buyback_factor),
        format::NBSP,
        format::date(q.date)
    )
}

/// The summary, the coins table and the add form: what htmx swaps after adding or removing.
#[derive(Template)]
#[template(path = "gold_panel.html")]
pub struct GoldPanel {
    pub summary: Summary,
    pub coins: Vec<CoinRow>,
    pub form: CoinForm,
    pub errors: FieldErrors,
}

impl GoldPanel {
    pub fn new(
        coins: &[Coin],
        price_per_gram: Option<Decimal>,
        buyback_factor: Decimal,
        form: CoinForm,
        errors: FieldErrors,
    ) -> GoldPanel {
        let values: Vec<_> = coins
            .iter()
            .map(|c| coin_value(c, price_per_gram, buyback_factor))
            .collect();
        GoldPanel {
            summary: Summary::new(coins, &values),
            coins: coins
                .iter()
                .zip(&values)
                .map(|(c, v)| CoinRow::new(c, *v))
                .collect(),
            form,
            errors,
        }
    }

    /// The weight or the quote has an error: their part of the form starts open.
    pub fn more_open(&self) -> bool {
        self.errors.fine_weight.is_some() || self.errors.manual_quote.is_some()
    }
}

#[derive(Template)]
#[template(path = "gold.html")]
pub struct GoldPage {
    pub shell: Shell,
    /// Gold is paused in the config: the "Purchases paused" badge.
    pub paused: bool,
    pub valued_at: String,
    /// The rendered [`GoldPanel`].
    pub panel: String,
}

/// The coins and today's NBP price per gram, when there is one.
async fn load(state: &AppState) -> Result<(Vec<Coin>, Option<PriceQuote>), AppError> {
    let coins = store::list_coins(&state.pool).await?;
    let clock = state.clock();
    let quote = match close_on_day(
        &state.pool,
        &state.market.nbp,
        &GOLD_GRAM,
        clock.today,
        clock,
    )
    .await
    {
        Ok(q) => Some(q),
        Err(e) => {
            eprintln!("gold: {e}");
            None
        }
    };
    Ok((coins, quote))
}

fn panel(
    state: &AppState,
    coins: &[Coin],
    quote: Option<&PriceQuote>,
    form: CoinForm,
    errors: FieldErrors,
) -> GoldPanel {
    GoldPanel::new(
        coins,
        quote.map(|q| q.close),
        state.market.gold_buyback_factor,
        form,
        errors,
    )
}

async fn page(
    state: &AppState,
    headers: &HeaderMap,
    config: &Config,
    quote: Option<&PriceQuote>,
    panel: GoldPanel,
) -> Result<GoldPage, AppError> {
    Ok(GoldPage {
        shell: Shell::load(state, headers, Page::Gold).await?,
        paused: config.is_paused(AssetClass::Gold),
        valued_at: valued_at(quote, state.market.gold_buyback_factor),
        panel: panel.render()?,
    })
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v == "true")
}

fn render(template: &impl Template) -> Result<Html<String>, AppError> {
    Ok(Html(template.render()?))
}

pub async fn show(
    State(state): State<AppState>,
    Extension(config): Extension<Arc<Config>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let (coins, quote) = load(&state).await?;
    let form = CoinForm::empty((state.today)());
    let p = panel(&state, &coins, quote.as_ref(), form, FieldErrors::default());
    let page = page(&state, &headers, &config, quote.as_ref(), p).await?;
    Ok(render(&page)?.into_response())
}

/// htmx requests get the refreshed panel; plain form posts are redirected back to the page
/// (or get the page with errors and 422).
pub async fn add(
    State(state): State<AppState>,
    Extension(config): Extension<Arc<Config>>,
    headers: HeaderMap,
    Form(form): Form<CoinForm>,
) -> Result<Response, AppError> {
    let result = parse_form(&form, (state.today)());
    if let Ok(coin) = &result {
        store::add_coin(&state.pool, coin).await?;
    }
    if result.is_ok() && !is_htmx(&headers) {
        return Ok(Redirect::to("/gold").into_response());
    }
    let (coins, quote) = load(&state).await?;
    let (form, errors) = match result {
        Ok(_) => (CoinForm::empty((state.today)()), FieldErrors::default()),
        Err(errors) => (form, errors),
    };
    let p = panel(&state, &coins, quote.as_ref(), form, errors);
    if is_htmx(&headers) {
        // htmx only swaps 2xx responses, so validation errors are a 200 panel there.
        return Ok(render(&p)?.into_response());
    }
    let page = page(&state, &headers, &config, quote.as_ref(), p).await?;
    Ok((StatusCode::UNPROCESSABLE_ENTITY, render(&page)?).into_response())
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
    let (coins, quote) = load(&state).await?;
    let form = CoinForm::empty((state.today)());
    let p = panel(&state, &coins, quote.as_ref(), form, FieldErrors::default());
    Ok(render(&p)?.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn plain(s: &str) -> String {
        s.replace(format::NBSP, " ").replace(format::MINUS, "-")
    }

    fn coin(id: i64, paid: Decimal, grams: Decimal, quote: Option<Decimal>) -> Coin {
        Coin {
            id,
            transaction_id: id,
            label: None,
            purchased_on: NaiveDate::from_ymd_opt(2025, 6, 12).unwrap(),
            purchase_price_pln: paid,
            fine_weight_g: grams,
            manual_quote_pln: quote,
        }
    }

    #[test]
    fn ounces_have_one_or_two_decimals() {
        // 3,11 / 31,1034768 = 0,09999 -> 0,10 -> "0,1"; twice that 0,19998 -> "0,2".
        assert_eq!(ounces_text(dec!(3.11)), "0,1");
        assert_eq!(ounces_text(dec!(6.22)), "0,2");
        // 1/20 oz: 1,555 / 31,1034768 = 0,049995 -> "0,05".
        assert_eq!(ounces_text(dec!(1.555)), "0,05");
        // 31,1035 g is one ounce: "1,0".
        assert_eq!(ounces_text(dec!(31.1035)), "1,0");
        assert_eq!(ounces_text(Decimal::ZERO), "0,0");
        assert_eq!(grams_text(dec!(3.11)), "3,11");
        assert_eq!(grams_text(dec!(7.7760)), "7,776");
    }

    #[test]
    fn coins_are_valued_at_the_nbp_price_or_their_quote() {
        // 400 zł/g x 3,11 g = 1244; x 0,95 = 1181,80.
        let nbp = coin(1, dec!(1100), dec!(3.11), None);
        assert_eq!(
            coin_value(&nbp, Some(dec!(400)), dec!(0.95)),
            Some((dec!(1181.80), ValuationBasis::NbpPrice))
        );
        // No NBP price: no value, unless the coin has its own dealer quote.
        assert_eq!(coin_value(&nbp, None, dec!(0.95)), None);
        let quoted = coin(2, dec!(1250), dec!(3.11), Some(dec!(1300)));
        for price in [Some(dec!(400)), None] {
            assert_eq!(
                coin_value(&quoted, price, dec!(0.95)),
                Some((dec!(1300), ValuationBasis::ManualQuote))
            );
        }
    }

    #[test]
    fn a_row_shows_the_value_and_the_pnl_against_what_was_paid() {
        // 1181,80 - 1100 = +81,80; 81,80 / 1100 = 7,436% -> "+7,4%".
        let gain = CoinRow::new(
            &coin(1, dec!(1100), dec!(3.11), None),
            Some((dec!(1181.80), ValuationBasis::NbpPrice)),
        );
        assert_eq!(gain.name, "1/10 oz coin");
        assert_eq!(plain(&gain.weight), "0,1 oz · 3,11 g fine gold");
        assert_eq!(gain.bought, "12.06.2025");
        assert_eq!(plain(&gain.paid), "1 100,00 zł");
        assert_eq!(plain(&gain.value), "1 181,80 zł");
        assert!(!gain.quoted);
        let p = gain.pnl.as_ref().unwrap();
        assert_eq!(
            (
                p.delta.arrow(),
                plain(&p.delta.text).as_str(),
                p.percent.as_str()
            ),
            ("▲", "+81,80 zł", "+7,4%")
        );
        assert_eq!(gain.remove_label(), "Remove 1/10 oz coin bought 12.06.2025");

        // 1181,80 - 1300 = -118,20; 118,20 / 1300 = 9,092% -> "-9,1%".
        let loss = CoinRow::new(
            &coin(2, dec!(1300), dec!(3.11), None),
            Some((dec!(1181.80), ValuationBasis::NbpPrice)),
        );
        let p = loss.pnl.unwrap();
        assert_eq!(
            (p.delta.arrow(), plain(&p.delta.text), plain(&p.percent)),
            ("▼", "-118,20 zł".into(), "-9,1%".into())
        );

        // Another weight is a "Gold coin"; without a value there is no P&L.
        let mut odd = coin(3, dec!(500), dec!(1.555), None);
        let none = CoinRow::new(&odd, None);
        assert_eq!(
            (none.name.as_str(), none.value.as_str()),
            ("Gold coin", "—")
        );
        assert_eq!(none.pnl, None);
        odd.label = Some("Krugerrand".into());
        assert_eq!(CoinRow::new(&odd, None).name, "Krugerrand");
    }

    #[test]
    fn the_summary_adds_the_coins_up() {
        // Valued 1181,80 (NBP) + 1300 (quote) = 2481,80; paid 1100 + 1250 = 2350;
        // +131,80, 131,80 / 2350 = 5,609% -> "+5,6%". 6,22 g = 0,2 oz.
        let coins = [
            coin(1, dec!(1100), dec!(3.11), None),
            coin(2, dec!(1250), dec!(3.11), Some(dec!(1300))),
        ];
        let values: Vec<_> = coins
            .iter()
            .map(|c| coin_value(c, Some(dec!(400)), dec!(0.95)))
            .collect();
        let s = Summary::new(&coins, &values);
        assert_eq!(plain(&s.value), "2 481,80 zł");
        assert_eq!(plain(&s.paid), "2 350,00 zł");
        let p = s.pnl.unwrap();
        assert_eq!(
            (plain(&p.delta.text), p.percent.as_str()),
            ("+131,80 zł".into(), "+5,6%")
        );
        assert_eq!(plain(&s.holding), "2 coins · 0,2 oz");

        // No NBP price: the quoted coin alone cannot give a total.
        let unpriced: Vec<_> = coins
            .iter()
            .map(|c| coin_value(c, None, dec!(0.95)))
            .collect();
        let s = Summary::new(&coins, &unpriced);
        assert_eq!((s.value.as_str(), s.pnl), ("—", None));
        assert_eq!(plain(&s.paid), "2 350,00 zł");

        // No coins: nothing paid, nothing worth, no P&L.
        let s = Summary::new(&[], &[]);
        assert_eq!(
            (plain(&s.value), plain(&s.paid), s.pnl, plain(&s.holding)),
            (
                "0,00 zł".into(),
                "0,00 zł".into(),
                None,
                "0 coins · 0,0 oz".into()
            )
        );
        let one = Summary::new(&coins[..1], &values[..1]);
        assert_eq!(plain(&one.holding), "1 coin · 0,1 oz");
    }

    #[test]
    fn the_header_says_how_coins_are_valued() {
        let quote = PriceQuote {
            date: NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
            close: dec!(400),
            currency: "PLN".into(),
        };
        // 400 x 3,11 x 0,95 = 1181,80 per coin.
        assert_eq!(
            plain(&valued_at(Some(&quote), dec!(0.95))),
            "Valued at NBP spot × 0,95 · 1 181,80 zł per 1/10 oz · 15.10.2026"
        );
        // A factor of 1 is spot itself: 400 x 3,11 = 1244.
        assert_eq!(
            plain(&valued_at(Some(&quote), Decimal::ONE)),
            "Valued at NBP spot · 1 244,00 zł per 1/10 oz · 15.10.2026"
        );
        // Three decimals are kept: 1244 x 0,975 = 1244 - 31,10 = 1212,90.
        assert_eq!(
            plain(&valued_at(Some(&quote), dec!(0.975))),
            "Valued at NBP spot × 0,975 · 1 212,90 zł per 1/10 oz · 15.10.2026"
        );
        assert_eq!(
            valued_at(None, dec!(0.95)),
            "No NBP gold price right now · coins with a dealer quote are valued at it"
        );
    }

    #[test]
    fn the_blank_form_is_for_today_and_a_tenth_of_an_ounce() {
        let f = CoinForm::empty(NaiveDate::from_ymd_opt(2026, 10, 16).unwrap());
        assert_eq!(
            (f.purchased_on.as_str(), f.fine_weight.as_str()),
            ("2026-10-16", "3,11")
        );
        // What the form shows is what it reads back.
        let typed = CoinForm {
            purchase_price: "1640".into(),
            ..f
        };
        let coin = parse_form(&typed, NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()).unwrap();
        assert_eq!(coin.fine_weight_g, TENTH_OUNCE_GRAMS);
    }
}
