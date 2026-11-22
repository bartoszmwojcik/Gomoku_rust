//! The `opponent` / `botLevel` fields of a create-game body: shape first, then the rules between the fields.

use super::choose_move::BotLevel;
use crate::paper::Field;
use crate::types::GameVariant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opponent {
    Human,
    Bot,
}

impl Opponent {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "human" => Some(Self::Human),
            "bot" => Some(Self::Bot),
            _ => None,
        }
    }
}

/// What is wrong with the opponent fields; the web layer words each issue (code and message).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpponentIssue {
    OpponentInvalid,
    LevelInvalid,
    LevelWithoutBot,
    LevelRequired,
    VariantUnsupported,
    PublicNotAllowed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpponentChoice {
    Human,
    Bot(BotLevel),
}

/// Resolves the fields. A shape problem is reported alone; otherwise the relations are checked in order: a level without a
/// bot (and nothing else), a bot without a level, a bot outside the classic variant, a public bot game.
pub fn resolve_opponent(
    opponent: Field<Opponent>,
    level: Field<BotLevel>,
    variant: GameVariant,
    is_public: bool,
) -> Result<OpponentChoice, Vec<OpponentIssue>> {
    let mut shape = Vec::new();
    if matches!(opponent, Field::Invalid) {
        shape.push(OpponentIssue::OpponentInvalid);
    }
    if matches!(level, Field::Invalid) {
        shape.push(OpponentIssue::LevelInvalid);
    }
    if !shape.is_empty() {
        return Err(shape);
    }
    let is_bot = matches!(opponent, Field::Valid(Opponent::Bot));
    if !is_bot {
        return if matches!(level, Field::Absent) {
            Ok(OpponentChoice::Human)
        } else {
            Err(vec![OpponentIssue::LevelWithoutBot])
        };
    }
    let mut issues = Vec::new();
    let chosen = match level {
        Field::Valid(level) => Some(level),
        _ => {
            issues.push(OpponentIssue::LevelRequired);
            None
        }
    };
    if variant != GameVariant::Classic {
        issues.push(OpponentIssue::VariantUnsupported);
    }
    if is_public {
        issues.push(OpponentIssue::PublicNotAllowed);
    }
    match (chosen, issues.is_empty()) {
        (Some(level), true) => Ok(OpponentChoice::Bot(level)),
        _ => Err(issues),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OpponentIssue::*;

    fn run(
        o: Field<Opponent>,
        l: Field<BotLevel>,
        v: GameVariant,
        p: bool,
    ) -> Result<OpponentChoice, Vec<OpponentIssue>> {
        resolve_opponent(o, l, v, p)
    }

    #[test]
    fn parses_opponents() {
        assert_eq!(Opponent::parse("human"), Some(Opponent::Human));
        assert_eq!(Opponent::parse("bot"), Some(Opponent::Bot));
        assert_eq!(Opponent::parse("Bot"), None);
    }

    #[test]
    fn humans() {
        let c = GameVariant::Classic;
        assert_eq!(
            run(Field::Absent, Field::Absent, c, false),
            Ok(OpponentChoice::Human)
        );
        assert_eq!(
            run(Field::Valid(Opponent::Human), Field::Absent, c, true),
            Ok(OpponentChoice::Human)
        );
        assert_eq!(
            run(
                Field::Valid(Opponent::Human),
                Field::Valid(BotLevel::One),
                c,
                false
            ),
            Err(vec![LevelWithoutBot])
        );
        assert_eq!(
            run(Field::Absent, Field::Valid(BotLevel::Two), c, false),
            Err(vec![LevelWithoutBot])
        );
    }

    #[test]
    fn shape_wins() {
        let c = GameVariant::Classic;
        assert_eq!(
            run(Field::Invalid, Field::Invalid, c, false),
            Err(vec![OpponentInvalid, LevelInvalid])
        );
        assert_eq!(
            run(
                Field::Valid(Opponent::Bot),
                Field::Invalid,
                GameVariant::Paper,
                true
            ),
            Err(vec![LevelInvalid])
        );
    }

    #[test]
    fn bots() {
        let c = GameVariant::Classic;
        let bot = Field::Valid(Opponent::Bot);
        assert_eq!(
            run(bot, Field::Valid(BotLevel::Two), c, false),
            Ok(OpponentChoice::Bot(BotLevel::Two))
        );
        assert_eq!(run(bot, Field::Absent, c, false), Err(vec![LevelRequired]));
        assert_eq!(
            run(bot, Field::Valid(BotLevel::One), GameVariant::Paper, false),
            Err(vec![VariantUnsupported])
        );
        assert_eq!(
            run(bot, Field::Valid(BotLevel::One), c, true),
            Err(vec![PublicNotAllowed])
        );
        assert_eq!(
            run(bot, Field::Absent, GameVariant::Paper, true),
            Err(vec![LevelRequired, VariantUnsupported, PublicNotAllowed])
        );
    }
}
