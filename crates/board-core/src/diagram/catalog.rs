//! 図に置ける部品の一覧(AWS サービス、AWS のグループ枠、フローチャート記号)。
//!
//! AWS 公式アイコンは再配布条件があるので同梱せず、公式のカテゴリ色と略称で表す。

use crate::color::Rgba8;

/// ノード(図の部品)の見た目の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeStyle {
    /// AWS サービスなど。カテゴリ色の枠の角丸四角形。
    Service,
    /// フローチャートの処理(四角形)。
    Process,
    /// 判断(楕円で代用)。
    Decision,
    /// 開始・終了(楕円)。
    Terminal,
    /// データ・入出力(四角形)。
    Data,
    /// 補足メモ(付箋)。
    Note,
}

/// ノードの種類 1 つ分。`id` は図の定義(JSON)に書く名前。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeKind {
    pub id: &'static str,
    /// 図形に表示する略称(AWS サービス名など)。空なら出さない。
    pub title: &'static str,
    pub category: Category,
    pub style: NodeStyle,
}

/// 色分けのカテゴリ(AWS アーキテクチャアイコンのカテゴリ色に合わせる)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Compute,
    Storage,
    Database,
    Networking,
    Security,
    Integration,
    Analytics,
    Management,
    MachineLearning,
    Frontend,
    DevTools,
    General,
    /// Microsoft Azure のサービス。
    Azure,
    /// Google Cloud のサービス。
    GoogleCloud,
    Flow,
}

impl Category {
    /// 枠線の色。
    pub fn color(self) -> Rgba8 {
        Rgba8::hex(match self {
            Category::Compute => 0xED7100,
            Category::Storage => 0x7AA116,
            Category::Database => 0xC925D1,
            Category::Networking | Category::Analytics => 0x8C4FFF,
            Category::Security | Category::Frontend => 0xDD344C,
            Category::Integration | Category::Management => 0xE7157B,
            Category::MachineLearning => 0x01A88D,
            Category::DevTools => 0x3F8624,
            Category::General => 0x232F3E,
            Category::Azure => 0x0078D4,
            Category::GoogleCloud => 0x4285F4,
            Category::Flow => 0x545B64,
        })
    }

    /// 塗りの色(枠の色をごく薄くしたもの)。
    pub fn fill(self) -> Rgba8 {
        match self {
            Category::Flow => Rgba8::WHITE,
            _ => tint(self.color(), 0.12),
        }
    }
}

/// `c` を白に混ぜる(`amount` が 1 で元の色)。
pub fn tint(c: Rgba8, amount: f32) -> Rgba8 {
    let mix = |v: u8| (255.0 - (255.0 - v as f32) * amount).round() as u8;
    Rgba8::rgb(mix(c.0[0]), mix(c.0[1]), mix(c.0[2]))
}

const fn svc(id: &'static str, title: &'static str, category: Category) -> NodeKind {
    NodeKind {
        id,
        title,
        category,
        style: NodeStyle::Service,
    }
}

const fn flow(id: &'static str, style: NodeStyle) -> NodeKind {
    NodeKind {
        id,
        title: "",
        category: Category::Flow,
        style,
    }
}

use Category::*;

/// 使えるノードの種類。並びはパレットの表示順。
pub const NODE_KINDS: &[NodeKind] = &[
    // 汎用(AWS の外側)
    svc("users", "ユーザー", General),
    svc("client", "クライアント", General),
    svc("mobile", "モバイル", General),
    svc("internet", "インターネット", General),
    svc("server", "サーバー", General),
    svc("on_premises", "オンプレミス", General),
    // コンピューティング・コンテナ
    svc("ec2", "EC2", Compute),
    svc("lambda", "Lambda", Compute),
    svc("ecs", "ECS", Compute),
    svc("eks", "EKS", Compute),
    svc("fargate", "Fargate", Compute),
    svc("ecr", "ECR", Compute),
    svc("batch", "Batch", Compute),
    svc("elastic_beanstalk", "Elastic Beanstalk", Compute),
    svc("lightsail", "Lightsail", Compute),
    svc("app_runner", "App Runner", Compute),
    // ストレージ
    svc("s3", "S3", Storage),
    svc("efs", "EFS", Storage),
    svc("ebs", "EBS", Storage),
    svc("s3_glacier", "S3 Glacier", Storage),
    svc("backup", "Backup", Storage),
    // データベース
    svc("rds", "RDS", Database),
    svc("aurora", "Aurora", Database),
    svc("dynamodb", "DynamoDB", Database),
    svc("elasticache", "ElastiCache", Database),
    svc("documentdb", "DocumentDB", Database),
    svc("neptune", "Neptune", Database),
    svc("redshift", "Redshift", Analytics),
    // ネットワーク・配信
    svc("cloudfront", "CloudFront", Networking),
    svc("route53", "Route 53", Networking),
    svc("api_gateway", "API Gateway", Networking),
    svc("alb", "ALB", Networking),
    svc("nlb", "NLB", Networking),
    svc("nat_gateway", "NAT Gateway", Networking),
    svc("internet_gateway", "Internet Gateway", Networking),
    svc("transit_gateway", "Transit Gateway", Networking),
    svc("vpc_endpoint", "VPC Endpoint", Networking),
    svc("direct_connect", "Direct Connect", Networking),
    svc("site_to_site_vpn", "Site-to-Site VPN", Networking),
    svc("global_accelerator", "Global Accelerator", Networking),
    // セキュリティ
    svc("iam", "IAM", Security),
    svc("cognito", "Cognito", Security),
    svc("waf", "WAF", Security),
    svc("shield", "Shield", Security),
    svc("kms", "KMS", Security),
    svc("secrets_manager", "Secrets Manager", Security),
    svc("acm", "Certificate Manager", Security),
    svc("guardduty", "GuardDuty", Security),
    // アプリケーション統合
    svc("sqs", "SQS", Integration),
    svc("sns", "SNS", Integration),
    svc("eventbridge", "EventBridge", Integration),
    svc("step_functions", "Step Functions", Integration),
    svc("appsync", "AppSync", Frontend),
    svc("amazon_mq", "Amazon MQ", Integration),
    // 分析
    svc("kinesis", "Kinesis", Analytics),
    svc("msk", "MSK", Analytics),
    svc("athena", "Athena", Analytics),
    svc("glue", "Glue", Analytics),
    svc("emr", "EMR", Analytics),
    svc("opensearch", "OpenSearch", Analytics),
    svc("quicksight", "QuickSight", Analytics),
    // 管理
    svc("cloudwatch", "CloudWatch", Management),
    svc("cloudtrail", "CloudTrail", Management),
    svc("cloudformation", "CloudFormation", Management),
    svc("systems_manager", "Systems Manager", Management),
    // 機械学習
    svc("bedrock", "Bedrock", MachineLearning),
    svc("sagemaker", "SageMaker", MachineLearning),
    svc("rekognition", "Rekognition", MachineLearning),
    svc("comprehend", "Comprehend", MachineLearning),
    // フロントエンド・開発
    svc("amplify", "Amplify", Frontend),
    svc("codepipeline", "CodePipeline", DevTools),
    svc("codebuild", "CodeBuild", DevTools),
    // Microsoft Azure
    svc("azure_vm", "Virtual Machines", Azure),
    svc("azure_vmss", "VM Scale Sets", Azure),
    svc("azure_app_service", "App Service", Azure),
    svc("azure_functions", "Functions", Azure),
    svc("azure_aks", "AKS", Azure),
    svc("azure_container_apps", "Container Apps", Azure),
    svc("azure_container_instances", "Container Instances", Azure),
    svc("azure_container_registry", "Container Registry", Azure),
    svc("azure_static_web_apps", "Static Web Apps", Azure),
    svc("azure_storage", "Storage Account", Azure),
    svc("azure_blob", "Blob Storage", Azure),
    svc("azure_files", "Azure Files", Azure),
    svc("azure_sql", "SQL Database", Azure),
    svc("azure_cosmos_db", "Cosmos DB", Azure),
    svc("azure_postgresql", "Database for PostgreSQL", Azure),
    svc("azure_mysql", "Database for MySQL", Azure),
    svc("azure_redis", "Cache for Redis", Azure),
    svc("azure_front_door", "Front Door", Azure),
    svc("azure_cdn", "CDN", Azure),
    svc("azure_application_gateway", "Application Gateway", Azure),
    svc("azure_load_balancer", "Load Balancer", Azure),
    svc("azure_dns", "DNS", Azure),
    svc("azure_firewall", "Firewall", Azure),
    svc("azure_nat_gateway", "NAT Gateway", Azure),
    svc("azure_vpn_gateway", "VPN Gateway", Azure),
    svc("azure_private_endpoint", "Private Endpoint", Azure),
    svc("azure_api_management", "API Management", Azure),
    svc("azure_entra_id", "Entra ID", Azure),
    svc("azure_key_vault", "Key Vault", Azure),
    svc("azure_service_bus", "Service Bus", Azure),
    svc("azure_event_grid", "Event Grid", Azure),
    svc("azure_event_hubs", "Event Hubs", Azure),
    svc("azure_logic_apps", "Logic Apps", Azure),
    svc("azure_data_factory", "Data Factory", Azure),
    svc("azure_synapse", "Synapse Analytics", Azure),
    svc("azure_databricks", "Databricks", Azure),
    svc("azure_monitor", "Monitor", Azure),
    svc("azure_app_insights", "Application Insights", Azure),
    svc("azure_log_analytics", "Log Analytics", Azure),
    svc("azure_openai", "Azure OpenAI", Azure),
    svc("azure_machine_learning", "Machine Learning", Azure),
    svc("azure_devops", "Azure DevOps", Azure),
    // Google Cloud
    svc("gcp_compute_engine", "Compute Engine", GoogleCloud),
    svc("gcp_gke", "GKE", GoogleCloud),
    svc("gcp_cloud_run", "Cloud Run", GoogleCloud),
    svc("gcp_cloud_functions", "Cloud Functions", GoogleCloud),
    svc("gcp_app_engine", "App Engine", GoogleCloud),
    svc("gcp_artifact_registry", "Artifact Registry", GoogleCloud),
    svc("gcp_cloud_storage", "Cloud Storage", GoogleCloud),
    svc("gcp_filestore", "Filestore", GoogleCloud),
    svc("gcp_cloud_sql", "Cloud SQL", GoogleCloud),
    svc("gcp_alloydb", "AlloyDB", GoogleCloud),
    svc("gcp_spanner", "Spanner", GoogleCloud),
    svc("gcp_firestore", "Firestore", GoogleCloud),
    svc("gcp_bigtable", "Bigtable", GoogleCloud),
    svc("gcp_memorystore", "Memorystore", GoogleCloud),
    svc("gcp_bigquery", "BigQuery", GoogleCloud),
    svc("gcp_pubsub", "Pub/Sub", GoogleCloud),
    svc("gcp_dataflow", "Dataflow", GoogleCloud),
    svc("gcp_dataproc", "Dataproc", GoogleCloud),
    svc("gcp_looker", "Looker", GoogleCloud),
    svc("gcp_load_balancing", "Cloud Load Balancing", GoogleCloud),
    svc("gcp_cloud_cdn", "Cloud CDN", GoogleCloud),
    svc("gcp_cloud_dns", "Cloud DNS", GoogleCloud),
    svc("gcp_cloud_nat", "Cloud NAT", GoogleCloud),
    svc("gcp_cloud_armor", "Cloud Armor", GoogleCloud),
    svc("gcp_cloud_vpn", "Cloud VPN", GoogleCloud),
    svc("gcp_api_gateway", "API Gateway", GoogleCloud),
    svc("gcp_apigee", "Apigee", GoogleCloud),
    svc("gcp_iam", "IAM", GoogleCloud),
    svc("gcp_secret_manager", "Secret Manager", GoogleCloud),
    svc("gcp_cloud_kms", "Cloud KMS", GoogleCloud),
    svc("gcp_cloud_scheduler", "Cloud Scheduler", GoogleCloud),
    svc("gcp_workflows", "Workflows", GoogleCloud),
    svc("gcp_cloud_logging", "Cloud Logging", GoogleCloud),
    svc("gcp_cloud_monitoring", "Cloud Monitoring", GoogleCloud),
    svc("gcp_vertex_ai", "Vertex AI", GoogleCloud),
    svc("gcp_cloud_build", "Cloud Build", GoogleCloud),
    // フローチャート
    flow("process", NodeStyle::Process),
    flow("decision", NodeStyle::Decision),
    flow("terminal", NodeStyle::Terminal),
    flow("data", NodeStyle::Data),
    flow("note", NodeStyle::Note),
];

/// 既定の種類(不明な名前もこれになる)。
pub const DEFAULT_NODE: &str = "process";

pub fn node_kind(id: &str) -> &'static NodeKind {
    let id = id.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    NODE_KINDS
        .iter()
        .find(|k| k.id == id)
        .unwrap_or_else(|| node_kind(DEFAULT_NODE))
}

/// グループ枠の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupKind {
    pub id: &'static str,
    /// 名前を省略したときの表示名。
    pub title: &'static str,
    color: u32,
    /// 薄い塗りを付けるか。
    filled: bool,
}

impl GroupKind {
    pub fn color(&self) -> Rgba8 {
        Rgba8::hex(self.color)
    }

    pub fn fill(&self) -> Rgba8 {
        if self.filled {
            tint(self.color(), 0.08)
        } else {
            Rgba8::WHITE
        }
    }
}

const fn group(id: &'static str, title: &'static str, color: u32, filled: bool) -> GroupKind {
    GroupKind {
        id,
        title,
        color,
        filled,
    }
}

pub const GROUP_KINDS: &[GroupKind] = &[
    group("aws_cloud", "AWS Cloud", 0x232F3E, false),
    group("region", "Region", 0x00A4A6, false),
    group("vpc", "VPC", 0x8C4FFF, false),
    group("availability_zone", "Availability Zone", 0x00A4A6, false),
    group("public_subnet", "Public subnet", 0x7AA116, true),
    group("private_subnet", "Private subnet", 0x00A4A6, true),
    group("security_group", "Security group", 0xDD344C, false),
    group("auto_scaling", "Auto Scaling group", 0xED7100, false),
    group("account", "AWS Account", 0xE7157B, false),
    group("on_premises", "オンプレミス", 0x7D8998, true),
    group("azure_subscription", "Azure Subscription", 0x0078D4, false),
    group("azure_resource_group", "Resource Group", 0x7D8998, false),
    group("azure_region", "Azure Region", 0x0078D4, false),
    group("azure_vnet", "Virtual Network", 0x50E6FF, false),
    group("azure_subnet", "Subnet", 0x0078D4, true),
    group("gcp_project", "Google Cloud Project", 0x4285F4, false),
    group("gcp_region", "Region", 0x34A853, false),
    group("gcp_zone", "Zone", 0xFBBC04, false),
    group("gcp_vpc", "VPC network", 0x4285F4, false),
    group("gcp_subnet", "Subnet", 0x4285F4, true),
    group("generic", "", 0x7D8998, false),
];

pub fn group_kind(id: &str) -> &'static GroupKind {
    let id = id.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    GROUP_KINDS
        .iter()
        .find(|k| k.id == id)
        .unwrap_or(&GROUP_KINDS[GROUP_KINDS.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_lenient() {
        assert_eq!(node_kind("EC2").title, "EC2");
        assert_eq!(node_kind("api-gateway").id, "api_gateway");
        assert_eq!(node_kind("???").id, DEFAULT_NODE);
        assert_eq!(group_kind("Public Subnet").id, "public_subnet");
        assert_eq!(group_kind("x").id, "generic");
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = NODE_KINDS.iter().map(|k| k.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), NODE_KINDS.len());
    }

    #[test]
    fn tint_mixes_with_white() {
        assert_eq!(tint(Rgba8::rgb(0, 0, 0), 0.0), Rgba8::WHITE);
        assert_eq!(tint(Rgba8::rgb(10, 20, 30), 1.0), Rgba8::rgb(10, 20, 30));
    }
}
