//! ホーム画面の「プラン」: 今のプラン・今月の利用状況・プランの比べと、申し込み・支払いの管理。
//!
//! 料金プランを使わないサーバー(`mode: off`)では何も出さない。申し込みと支払いの管理は
//! サーバーが返す URL(Stripe Checkout・カスタマーポータル。テストモードならこのサーバーの画面)へ移る。

use board_core::api::{Billing, BillingRedirect, PlanId, PlanLimits};

use crate::http::{self, HttpError, escape};

/// 料金プランの情報。古いサーバーや料金プランを使わないサーバーなら `None`。
pub async fn load() -> Option<Billing> {
    http::get::<Billing>("/billing")
        .await
        .ok()
        .filter(|b| b.mode != "off")
}

/// 申し込み(`checkout`)か支払いの管理(`portal`)の画面の URL をもらう。
pub async fn start(kind: &str) -> Result<String, HttpError> {
    http::send::<_, BillingRedirect>("POST", &format!("/billing/{kind}"), &serde_json::json!({}))
        .await
        .map(|r| r.url)
}

/// 決済の画面から戻ってきたときの知らせ(`?billing=`)。
pub fn notice_for(param: &str) -> Option<&'static str> {
    match param {
        "success" => Some("お申し込みありがとうございます。プロプランになりました。"),
        "cancel" => Some("申し込みを取りやめました。プランは変わっていません。"),
        "canceled" => Some("解約しました。無料プランに戻りました。"),
        _ => None,
    }
}

pub fn plan_label(plan: PlanId) -> &'static str {
    match plan {
        PlanId::Free => "無料",
        PlanId::Pro => "プロ",
    }
}

/// ヘッダーのボタン。
pub fn header_button(b: &Billing, open: bool) -> String {
    format!(
        r#"<button type="button" class="quiet plan-chip" data-action="plan" data-plan="{id}" aria-expanded="{open}">プラン: {label}</button>"#,
        id = b.plan.as_str(),
        label = plan_label(b.plan),
    )
}

fn limit(n: u32, unit: &str) -> String {
    if n == 0 {
        "無制限".into()
    } else {
        format!("{n} {unit}")
    }
}

fn limits_list(l: &PlanLimits) -> String {
    format!(
        "<ul><li>ボード {}</li><li>AI 機能 {}</li><li>ボードのメンバー {}</li><li>動画メモ {}</li></ul>",
        limit(l.boards, "個"),
        if l.ai_per_month == 0 {
            "無制限".into()
        } else {
            format!("月 {} 回", l.ai_per_month)
        },
        limit(l.members_per_board, "人まで"),
        limit(l.media_mb_per_board, "MB まで"),
    )
}

/// 使った量と上限のメーター。
fn meter(label: &str, used: u32, max: u32) -> String {
    let (pct, text, full) = if max == 0 {
        (0.0, format!("{used} / 無制限"), false)
    } else {
        (
            (f64::from(used) / f64::from(max) * 100.0).min(100.0),
            format!("{used} / {max}"),
            used >= max,
        )
    };
    format!(
        r#"<div class="plan-meter" data-full="{full}"><span>{label}</span><span class="nums">{text}</span><div class="bar"><i style="width:{pct:.0}%"></i></div></div>"#
    )
}

fn date(ms: u64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms as f64));
    format!(
        "{}年{}月{}日",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date()
    )
}

/// プランのパネル。
pub fn panel(b: &Billing, busy: bool) -> String {
    let mut cards = String::new();
    for offer in &b.plans {
        let current = offer.id == b.plan;
        let action = match (offer.id, b.plan) {
            (PlanId::Pro, PlanId::Free) => format!(
                r#"<button type="button" class="primary" data-action="plan-upgrade" {}>プロにアップグレード</button>"#,
                if busy { "disabled" } else { "" }
            ),
            (PlanId::Pro, PlanId::Pro) if b.can_manage => format!(
                r#"<button type="button" data-action="plan-manage" {}>お支払いの管理</button>"#,
                if busy { "disabled" } else { "" }
            ),
            _ if current => r#"<span class="muted small">今のプラン</span>"#.into(),
            _ => String::new(),
        };
        cards.push_str(&format!(
            r#"<div class="plan-card{cls}" data-plan="{id}"><div class="plan-name">{name}{badge}</div><div class="plan-price">{price}</div>{limits}{action}</div>"#,
            cls = if current { " current" } else { "" },
            id = offer.id.as_str(),
            name = escape(&offer.name),
            badge = if current {
                r#" <span class="badge" data-role="owner">利用中</span>"#
            } else {
                ""
            },
            price = escape(&offer.price),
            limits = limits_list(&offer.limits),
        ));
    }
    let mut notes = String::new();
    match b.status.as_deref() {
        Some("past_due") => notes.push_str(
            r#"<p class="plan-note warn">お支払いを確認できていません。「お支払いの管理」から支払い方法を更新してください。</p>"#,
        ),
        _ if b.plan == PlanId::Pro && b.cancel_at_period_end => {
            if let Some(end) = b.period_end {
                notes.push_str(&format!(
                    r#"<p class="plan-note">{} に無料プランに戻ります(解約の予定)。</p>"#,
                    date(end)
                ));
            }
        }
        _ if b.plan == PlanId::Pro => {
            if let Some(end) = b.period_end {
                notes.push_str(&format!(
                    r#"<p class="muted small">次の更新日: {}</p>"#,
                    date(end)
                ));
            }
        }
        _ => {}
    }
    if b.mode == "mock" {
        notes.push_str(
            r#"<p class="plan-note test">テストモードです。申し込んでも実際の請求は発生しません。</p>"#,
        );
    }
    format!(
        r#"<section class="team-panel plan-panel" aria-label="料金プラン">
  <header><h2>料金プラン</h2><span class="spacer"></span><button type="button" class="quiet" data-action="plan">閉じる</button></header>
  <div class="plan-usage">{boards}{ai}</div>
  <div class="plan-cards">{cards}</div>
  {notes}
  <p class="muted small">プランはボードのオーナーに付きます。メンバー数・動画メモはボードごと、AI 機能はオーナーの月ごとの回数で数えます。プランを下げても、今あるボードはそのまま残ります。</p>
</section>"#,
        boards = meter("ボード", b.usage.boards, b.limits.boards),
        ai = meter(
            "今月の AI 機能",
            b.usage.ai_this_month,
            b.limits.ai_per_month
        ),
    )
}
