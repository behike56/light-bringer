use serde::{Deserialize, Serialize};

macro_rules! stored_enum {
    ($(#[$enum_attr:meta])* $name:ident ($field:literal) { $($(#[$attr:meta])* $variant:ident => $key:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
        $(#[$enum_attr])*
        pub enum $name {
            $($(#[$attr])* #[serde(rename = $key)] $variant),+
        }

        impl $name {
            pub fn key(self) -> &'static str {
                match self {
                    $(Self::$variant => $key),+
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                match value.as_str() {
                    $($key => Ok(Self::$variant),)+
                    _ => Err(serde::de::Error::custom(format!("{} に未対応の値「{value}」があります", $field))),
                }
            }
        }
    };
}

stored_enum! {
    #[derive(Default)]
    ProjectMode("mode") {
        #[default]
        Free => "free",
        Ddd => "ddd",
    }
}

impl ProjectMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Free => "フリーモード",
            Self::Ddd => "DDDモード",
        }
    }
}

stored_enum! {
    CardKind("event_storming_kind") {
        ViewRead => "view_read",
        Actor => "actor",
        Command => "command",
        Aggregate => "aggregate",
        DomainEvent => "domain_event",
        Policy => "policy",
        ExternalSystem => "external_system",
    }
}

impl CardKind {
    pub const ALL: [Self; 7] = [
        Self::ViewRead,
        Self::Actor,
        Self::Command,
        Self::Aggregate,
        Self::DomainEvent,
        Self::Policy,
        Self::ExternalSystem,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ViewRead => "ビュー/リード",
            Self::Actor => "アクター",
            Self::Command => "コマンド",
            Self::Aggregate => "集約",
            Self::DomainEvent => "ドメインイベント",
            Self::Policy => "ポリシー",
            Self::ExternalSystem => "外部システム",
        }
    }

    pub fn name_example(self) -> &'static str {
        match self {
            Self::ViewRead => "注文画面",
            Self::Actor => "会員",
            Self::Command => "書籍を注文する",
            Self::Aggregate => "注文",
            Self::DomainEvent => "書籍が注文された",
            Self::Policy => "注文受付後に決済を依頼する",
            Self::ExternalSystem => "Stripe",
        }
    }
}

pub fn card_label(kind: Option<CardKind>) -> &'static str {
    kind.map_or("未分類", CardKind::label)
}

stored_enum! {
    DddRelation("event_storming_relation") {
        Informs => "informs",
        Issues => "issues",
        HandledBy => "handled_by",
        Emits => "emits",
        Triggers => "triggers",
        Calls => "calls",
        Reports => "reports",
        Updates => "updates",
    }
}

impl DddRelation {
    pub fn for_ends(source: Option<CardKind>, target: Option<CardKind>) -> Option<Self> {
        use CardKind::*;

        Some(match (source?, target?) {
            (ViewRead, Actor) => Self::Informs,
            (Actor | Policy, Command) => Self::Issues,
            (Command, Aggregate) => Self::HandledBy,
            (Aggregate, DomainEvent) => Self::Emits,
            (DomainEvent, Policy) => Self::Triggers,
            (Command, ExternalSystem) => Self::Calls,
            (ExternalSystem, DomainEvent) => Self::Reports,
            (DomainEvent, ViewRead) => Self::Updates,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Informs => "情報を提示する",
            Self::Issues => "操作を指示する",
            Self::HandledBy => "集約に操作を要求する",
            Self::Emits => "イベントを発行する",
            Self::Triggers => "判断のきっかけになる",
            Self::Calls => "処理を依頼する",
            Self::Reports => "結果を報告する",
            Self::Updates => "参照情報に反映する",
        }
    }
}
