//! Deterministic FSRS scheduling adapter.
//!
//! Lumi stores the full resulting state and algorithm version so a future
//! adapter can replay immutable attempts without rewriting history.

use lumi_core::{
    LearningAttemptOutcome, LearningReviewRating, LearningSchedule, LearningScheduleState,
    LearningValidationError, Scheduler, SchedulerDecision, SchedulerReview, SelfCheckRating,
};
use serde_json::json;

const ALGORITHM: &str = "fsrs";
const VERSION: &str = "fsrs-4.5-lumi-v1";
const DESIRED_RETENTION: f64 = 0.9;
const DAY_MS: u64 = 86_400_000;
// Published FSRS-4.5 defaults. Keeping them local makes replay reproducible
// and avoids coupling durable state to one third-party crate API.
const W: [f64; 17] = [
    0.4, 0.6, 2.4, 5.8, 4.93, 0.94, 0.86, 0.01, 1.49, 0.14, 0.94, 2.18, 0.05, 0.34, 1.26, 0.29,
    2.61,
];

/// Reproducible FSRS-4.5 adapter used by the scheduling vertical.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FsrsScheduler;

impl Scheduler for FsrsScheduler {
    fn review(
        &self,
        review: SchedulerReview,
    ) -> Result<SchedulerDecision, LearningValidationError> {
        if review
            .previous
            .as_ref()
            .is_some_and(|schedule| schedule.state == LearningScheduleState::Paused)
        {
            return Err(LearningValidationError::InvalidSchedule);
        }

        let suggested_rating = suggested_rating(&review);
        let rating_index = rating_index(review.rating);
        let previous = review.previous.as_ref();
        let elapsed_days = previous
            .and_then(|schedule| schedule.last_review_at)
            .map_or(0.0, |last| {
                review.reviewed_at.saturating_sub(last) as f64 / DAY_MS as f64
            });
        let (stability, difficulty, state, lapses) = match previous {
            None => (
                W[rating_index],
                initial_difficulty(review.rating),
                initial_state(review.rating),
                u32::from(review.rating == LearningReviewRating::Again),
            ),
            Some(schedule) => {
                let retrievability = retrievability(schedule.stability.max(0.01), elapsed_days);
                let stability = if review.rating == LearningReviewRating::Again {
                    next_forget_stability(schedule.difficulty, schedule.stability, retrievability)
                } else {
                    next_recall_stability(
                        schedule.difficulty,
                        schedule.stability,
                        retrievability,
                        review.rating,
                    )
                };
                (
                    stability,
                    next_difficulty(schedule.difficulty, review.rating),
                    if review.rating == LearningReviewRating::Again {
                        LearningScheduleState::Relearning
                    } else {
                        LearningScheduleState::Review
                    },
                    schedule.lapses + u32::from(review.rating == LearningReviewRating::Again),
                )
            }
        };
        if !stability.is_finite() || !difficulty.is_finite() {
            return Err(LearningValidationError::InvalidSchedule);
        }

        let scheduled_days = next_interval_days(stability, review.rating);
        let due_at = review
            .reviewed_at
            .saturating_add(scheduled_days.saturating_mul(DAY_MS));
        let repetitions = previous.map_or(1, |schedule| schedule.repetitions.saturating_add(1));
        let object_revision =
            previous.map_or(1, |schedule| schedule.object_revision.saturating_add(1));
        let schedule = LearningSchedule {
            item_id: previous.map_or(uuid::Uuid::nil(), |value| value.item_id),
            state,
            due_at,
            stability,
            difficulty,
            last_review_at: Some(review.reviewed_at),
            repetitions,
            lapses,
            algorithm: ALGORITHM.to_owned(),
            algorithm_version: VERSION.to_owned(),
            algorithm_payload: json!({
                "desired_retention": DESIRED_RETENTION,
                "elapsed_days": elapsed_days,
                "scheduled_days": scheduled_days,
                "rating": review.rating,
                "suggested_rating": suggested_rating,
                "assisted": review.source_opened || !review.hints_used.is_empty(),
                "weights": W,
            }),
            object_revision,
            paused_at: None,
        };
        Ok(SchedulerDecision {
            schedule,
            suggested_rating,
        })
    }
}

impl FsrsScheduler {
    /// Schedule one concrete item while keeping the platform-independent port
    /// free from a redundant item-id argument.
    pub(crate) fn review_item(
        &self,
        item_id: uuid::Uuid,
        review: SchedulerReview,
    ) -> Result<SchedulerDecision, LearningValidationError> {
        let mut decision = self.review(review)?;
        decision.schedule.item_id = item_id;
        Ok(decision)
    }
}

fn rating_index(rating: LearningReviewRating) -> usize {
    match rating {
        LearningReviewRating::Again => 0,
        LearningReviewRating::Hard => 1,
        LearningReviewRating::Good => 2,
        LearningReviewRating::Easy => 3,
    }
}

fn rating_number(rating: LearningReviewRating) -> f64 {
    (rating_index(rating) + 1) as f64
}

fn initial_difficulty(rating: LearningReviewRating) -> f64 {
    (W[4] - W[5] * (rating_number(rating) - 1.0)).clamp(1.0, 10.0)
}

fn initial_state(rating: LearningReviewRating) -> LearningScheduleState {
    if rating == LearningReviewRating::Again {
        LearningScheduleState::Learning
    } else {
        LearningScheduleState::Review
    }
}

fn next_difficulty(difficulty: f64, rating: LearningReviewRating) -> f64 {
    let shifted = difficulty - W[6] * (rating_number(rating) - 3.0);
    (W[7] * initial_difficulty(LearningReviewRating::Again) + (1.0 - W[7]) * shifted)
        .clamp(1.0, 10.0)
}

fn retrievability(stability: f64, elapsed_days: f64) -> f64 {
    (1.0 + elapsed_days.max(0.0) / (9.0 * stability)).powf(-1.0)
}

fn next_recall_stability(
    difficulty: f64,
    stability: f64,
    retrievability: f64,
    rating: LearningReviewRating,
) -> f64 {
    let hard_penalty = if rating == LearningReviewRating::Hard {
        W[15]
    } else {
        1.0
    };
    let easy_bonus = if rating == LearningReviewRating::Easy {
        W[16]
    } else {
        1.0
    };
    stability
        * (1.0
            + W[8].exp()
                * (11.0 - difficulty)
                * stability.powf(-W[9])
                * (((1.0 - retrievability) * W[10]).exp() - 1.0)
                * hard_penalty
                * easy_bonus)
}

fn next_forget_stability(difficulty: f64, stability: f64, retrievability: f64) -> f64 {
    (W[11]
        * difficulty.powf(-W[12])
        * ((stability + 1.0).powf(W[13]) - 1.0)
        * ((1.0 - retrievability) * W[14]).exp())
    .max(0.01)
}

fn next_interval_days(stability: f64, rating: LearningReviewRating) -> u64 {
    if rating == LearningReviewRating::Again {
        return 1;
    }
    let interval = stability * 9.0 * (1.0 / DESIRED_RETENTION - 1.0);
    interval.round().clamp(1.0, 36_500.0) as u64
}

fn suggested_rating(review: &SchedulerReview) -> LearningReviewRating {
    if review.outcome == LearningAttemptOutcome::Incorrect
        || review.self_check == Some(SelfCheckRating::NotRecalled)
    {
        LearningReviewRating::Again
    } else if review.source_opened
        || !review.hints_used.is_empty()
        || review.self_check == Some(SelfCheckRating::Partial)
    {
        LearningReviewRating::Hard
    } else {
        LearningReviewRating::Good
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(rating: LearningReviewRating) -> SchedulerReview {
        SchedulerReview {
            previous: None,
            rating,
            outcome: LearningAttemptOutcome::Correct,
            self_check: None,
            source_opened: false,
            hints_used: Vec::new(),
            reviewed_at: 1_000_000_000,
        }
    }

    #[test]
    fn initial_regression_vector_orders_intervals_by_rating() -> Result<(), LearningValidationError>
    {
        let scheduler = FsrsScheduler;
        let hard = scheduler.review_item(uuid::Uuid::nil(), review(LearningReviewRating::Hard))?;
        let good = scheduler.review_item(uuid::Uuid::nil(), review(LearningReviewRating::Good))?;
        let easy = scheduler.review_item(uuid::Uuid::nil(), review(LearningReviewRating::Easy))?;

        assert!(hard.schedule.due_at < good.schedule.due_at);
        assert!(good.schedule.due_at < easy.schedule.due_at);
        Ok(())
    }

    #[test]
    fn assisted_correct_recall_suggests_hard() -> Result<(), LearningValidationError> {
        let mut evidence = review(LearningReviewRating::Good);
        evidence.hints_used.push(1);

        let decision = FsrsScheduler.review_item(uuid::Uuid::nil(), evidence)?;

        assert_eq!(decision.suggested_rating, LearningReviewRating::Hard);
        Ok(())
    }

    #[test]
    fn again_increments_lapses_without_erasing_previous_state(
    ) -> Result<(), LearningValidationError> {
        let item_id = uuid::Uuid::now_v7();
        let initial = FsrsScheduler.review_item(item_id, review(LearningReviewRating::Good))?;
        let mut next = review(LearningReviewRating::Again);
        next.previous = Some(initial.schedule);
        next.reviewed_at += 10 * DAY_MS;

        let decision = FsrsScheduler.review_item(item_id, next)?;

        assert_eq!(decision.schedule.state, LearningScheduleState::Relearning);
        assert_eq!(decision.schedule.repetitions, 2);
        assert_eq!(decision.schedule.lapses, 1);
        Ok(())
    }
}
