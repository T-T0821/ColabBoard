//! すぐ使える図のサンプル(生成 AI と同じ JSON 形式)。

use super::DiagramSpec;

/// (表示名, JSON)
pub const SAMPLES: &[(&str, &str)] = &[
    ("AWS: Web 3 層構成", AWS_THREE_TIER),
    ("AWS: サーバーレス API", AWS_SERVERLESS),
    ("Azure: Web アプリ", AZURE_WEB),
    ("Google Cloud: コンテナ API", GCP_CONTAINER),
    ("フローチャート: 問い合わせ対応", FLOW_SUPPORT),
];

const AWS_THREE_TIER: &str = r#"{
  "title": "Web 3 層構成",
  "direction": "right",
  "groups": [
    {"id": "cloud", "kind": "aws_cloud", "label": "AWS Cloud"},
    {"id": "region", "kind": "region", "label": "ap-northeast-1", "parent": "cloud"},
    {"id": "vpc", "kind": "vpc", "label": "VPC 10.0.0.0/16", "parent": "region"},
    {"id": "pub", "kind": "public_subnet", "label": "Public subnet", "parent": "vpc"},
    {"id": "app", "kind": "private_subnet", "label": "Private subnet (App)", "parent": "vpc"},
    {"id": "db", "kind": "private_subnet", "label": "Private subnet (DB)", "parent": "vpc"}
  ],
  "nodes": [
    {"id": "users", "kind": "users", "label": "利用者"},
    {"id": "dns", "kind": "route53", "label": "", "group": "cloud"},
    {"id": "cdn", "kind": "cloudfront", "label": "", "group": "cloud"},
    {"id": "alb", "kind": "alb", "label": "", "group": "pub"},
    {"id": "web1", "kind": "ec2", "label": "Web/App #1", "group": "app"},
    {"id": "web2", "kind": "ec2", "label": "Web/App #2", "group": "app"},
    {"id": "rds", "kind": "aurora", "label": "MySQL", "group": "db"},
    {"id": "cache", "kind": "elasticache", "label": "Redis", "group": "db"},
    {"id": "assets", "kind": "s3", "label": "静的ファイル", "group": "region"}
  ],
  "edges": [
    {"from": "users", "to": "dns", "label": "名前解決"},
    {"from": "users", "to": "cdn", "label": "HTTPS"},
    {"from": "cdn", "to": "alb"},
    {"from": "cdn", "to": "assets"},
    {"from": "alb", "to": "web1"},
    {"from": "alb", "to": "web2"},
    {"from": "web1", "to": "rds"},
    {"from": "web2", "to": "rds"},
    {"from": "web1", "to": "cache"}
  ]
}"#;

const AWS_SERVERLESS: &str = r#"{
  "title": "サーバーレス API",
  "direction": "right",
  "groups": [
    {"id": "cloud", "kind": "aws_cloud", "label": "AWS Cloud"}
  ],
  "nodes": [
    {"id": "app", "kind": "mobile", "label": "アプリ"},
    {"id": "auth", "kind": "cognito", "label": "認証", "group": "cloud"},
    {"id": "api", "kind": "api_gateway", "label": "REST API", "group": "cloud"},
    {"id": "fn", "kind": "lambda", "label": "業務ロジック", "group": "cloud"},
    {"id": "table", "kind": "dynamodb", "label": "注文", "group": "cloud"},
    {"id": "queue", "kind": "sqs", "label": "通知キュー", "group": "cloud"},
    {"id": "notify", "kind": "lambda", "label": "通知送信", "group": "cloud"},
    {"id": "mail", "kind": "sns", "label": "", "group": "cloud"},
    {"id": "logs", "kind": "cloudwatch", "label": "ログ・監視", "group": "cloud"}
  ],
  "edges": [
    {"from": "app", "to": "auth", "label": "サインイン"},
    {"from": "app", "to": "api"},
    {"from": "api", "to": "fn"},
    {"from": "fn", "to": "table"},
    {"from": "fn", "to": "queue"},
    {"from": "queue", "to": "notify"},
    {"from": "notify", "to": "mail"},
    {"from": "fn", "to": "logs"}
  ]
}"#;

const AZURE_WEB: &str = r#"{
  "title": "Azure Web アプリ",
  "direction": "right",
  "groups": [
    {"id": "sub", "kind": "azure_subscription", "label": ""},
    {"id": "rg", "kind": "azure_resource_group", "label": "rg-web-prod", "parent": "sub"},
    {"id": "vnet", "kind": "azure_vnet", "label": "vnet-web", "parent": "rg"},
    {"id": "app", "kind": "azure_subnet", "label": "snet-app", "parent": "vnet"},
    {"id": "data", "kind": "azure_subnet", "label": "snet-data", "parent": "vnet"}
  ],
  "nodes": [
    {"id": "users", "kind": "users", "label": "利用者"},
    {"id": "fd", "kind": "azure_front_door", "label": "", "group": "sub"},
    {"id": "web", "kind": "azure_app_service", "label": "Web", "group": "app"},
    {"id": "fn", "kind": "azure_functions", "label": "バッチ", "group": "app"},
    {"id": "sql", "kind": "azure_sql", "label": "", "group": "data"},
    {"id": "redis", "kind": "azure_redis", "label": "セッション", "group": "data"},
    {"id": "kv", "kind": "azure_key_vault", "label": "", "group": "rg"},
    {"id": "insights", "kind": "azure_app_insights", "label": "", "group": "rg"}
  ],
  "edges": [
    {"from": "users", "to": "fd", "label": "HTTPS"},
    {"from": "fd", "to": "web"},
    {"from": "web", "to": "sql"},
    {"from": "web", "to": "redis"},
    {"from": "fn", "to": "sql"},
    {"from": "web", "to": "kv"},
    {"from": "web", "to": "insights"}
  ]
}"#;

const GCP_CONTAINER: &str = r#"{
  "title": "Google Cloud コンテナ API",
  "direction": "right",
  "groups": [
    {"id": "proj", "kind": "gcp_project", "label": "my-project"},
    {"id": "region", "kind": "gcp_region", "label": "asia-northeast1", "parent": "proj"},
    {"id": "vpc", "kind": "gcp_vpc", "label": "", "parent": "region"}
  ],
  "nodes": [
    {"id": "client", "kind": "mobile", "label": "アプリ"},
    {"id": "lb", "kind": "gcp_load_balancing", "label": "", "group": "proj"},
    {"id": "armor", "kind": "gcp_cloud_armor", "label": "", "group": "proj"},
    {"id": "run", "kind": "gcp_cloud_run", "label": "API", "group": "region"},
    {"id": "sql", "kind": "gcp_cloud_sql", "label": "PostgreSQL", "group": "vpc"},
    {"id": "redis", "kind": "gcp_memorystore", "label": "キャッシュ", "group": "vpc"},
    {"id": "topic", "kind": "gcp_pubsub", "label": "イベント", "group": "region"},
    {"id": "bq", "kind": "gcp_bigquery", "label": "分析", "group": "proj"}
  ],
  "edges": [
    {"from": "client", "to": "lb", "label": "HTTPS"},
    {"from": "lb", "to": "armor"},
    {"from": "armor", "to": "run"},
    {"from": "run", "to": "sql"},
    {"from": "run", "to": "redis"},
    {"from": "run", "to": "topic"},
    {"from": "topic", "to": "bq"}
  ]
}"#;

const FLOW_SUPPORT: &str = r#"{
  "title": "問い合わせ対応",
  "direction": "down",
  "nodes": [
    {"id": "start", "kind": "terminal", "label": "問い合わせ受付"},
    {"id": "faq", "kind": "decision", "label": "FAQ で解決?"},
    {"id": "reply", "kind": "process", "label": "FAQ のリンクを返信"},
    {"id": "ticket", "kind": "data", "label": "チケット起票"},
    {"id": "urgent", "kind": "decision", "label": "緊急?"},
    {"id": "escalate", "kind": "process", "label": "担当者へ電話連絡"},
    {"id": "queue", "kind": "process", "label": "通常キューで対応"},
    {"id": "end", "kind": "terminal", "label": "完了"},
    {"id": "memo", "kind": "note", "label": "SLA: 緊急は 1 時間以内"}
  ],
  "edges": [
    {"from": "start", "to": "faq"},
    {"from": "faq", "to": "reply", "label": "はい"},
    {"from": "faq", "to": "ticket", "label": "いいえ"},
    {"from": "ticket", "to": "urgent"},
    {"from": "urgent", "to": "escalate", "label": "はい"},
    {"from": "urgent", "to": "queue", "label": "いいえ"},
    {"from": "reply", "to": "end"},
    {"from": "escalate", "to": "end"},
    {"from": "queue", "to": "end"}
  ]
}"#;

pub fn aws_three_tier() -> DiagramSpec {
    DiagramSpec::from_json(AWS_THREE_TIER).expect("サンプルは正しい JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_parse_and_reference_known_kinds() {
        use super::super::catalog;
        for (name, json) in SAMPLES {
            let s = DiagramSpec::from_json(json).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!s.nodes.is_empty());
            for n in &s.nodes {
                assert_eq!(catalog::node_kind(&n.kind).id, n.kind, "{name}: {}", n.kind);
            }
            for g in &s.groups {
                assert_eq!(
                    catalog::group_kind(&g.kind).id,
                    g.kind,
                    "{name}: {}",
                    g.kind
                );
            }
        }
    }
}
