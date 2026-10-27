use std::time::Duration;

use crate::assets::HexAtlas;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum Animation {
    Idle,
    Working,
    Waiting,
    Succeeded,
    Failed,
    Review,
    Waving,
    RunningLeft,
    RunningRight,
}

impl Animation {
    pub(crate) const ALL: [Self; 9] = [
        Self::Idle,
        Self::Working,
        Self::Waiting,
        Self::Succeeded,
        Self::Failed,
        Self::Review,
        Self::Waving,
        Self::RunningLeft,
        Self::RunningRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Review => "review",
            Self::Waving => "waving",
            Self::RunningLeft => "running-left",
            Self::RunningRight => "running-right",
        }
    }
}

pub struct Player {
    animation: Animation,
    frame_index: usize,
    elapsed: Duration,
}

impl Player {
    pub fn new(animation: Animation) -> Self {
        Self {
            animation,
            frame_index: 0,
            elapsed: Duration::ZERO,
        }
    }

    pub fn select(&mut self, animation: Animation) {
        *self = Self::new(animation);
    }

    pub fn animation(&self) -> Animation {
        self.animation
    }

    pub fn frame_index(&self) -> usize {
        self.frame_index
    }

    /// Returns whether the final visible frame differs, not whether time advanced.
    pub fn advance(&mut self, delta: Duration, atlas: &HexAtlas) -> bool {
        let before = (self.animation, self.frame_index);
        let clip = atlas.clip(self.animation);
        // Work is bounded by clip length, even for Duration::MAX or long stalls.
        let mut position = clip.frame_durations[..self.frame_index]
            .iter()
            .map(Duration::as_nanos)
            .sum::<u128>()
            + self.elapsed.as_nanos()
            + delta.as_nanos();
        let total = clip
            .frame_durations
            .iter()
            .map(Duration::as_nanos)
            .sum::<u128>();
        if !clip.looping && position >= total {
            self.animation = Animation::Idle;
            position -= total;
        }
        let clip = atlas.clip(self.animation);
        let total = clip
            .frame_durations
            .iter()
            .map(Duration::as_nanos)
            .sum::<u128>();
        position %= total;
        for (index, duration) in clip.frame_durations.iter().enumerate() {
            if position < duration.as_nanos() {
                self.frame_index = index;
                // Position is now bounded by one validated frame duration.
                self.elapsed = Duration::new(
                    (position / 1_000_000_000) as u64,
                    (position % 1_000_000_000) as u32,
                );
                break;
            }
            position -= duration.as_nanos();
        }
        before != (self.animation, self.frame_index)
    }

    pub fn until_next_frame(&self, atlas: &HexAtlas) -> Duration {
        atlas.clip(self.animation).frame_durations[self.frame_index] - self.elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_cli_and_manifest_names() {
        use clap::ValueEnum;
        for animation in Animation::ALL {
            assert_eq!(
                Animation::from_str(animation.label(), false).unwrap(),
                animation
            );
            let json = format!("\"{}\"", animation.label());
            assert_eq!(serde_json::from_str::<Animation>(&json).unwrap(), animation);
        }
    }

    #[test]
    fn exact_boundaries_and_looping() {
        let atlas = HexAtlas::load().unwrap();
        let mut player = Player::new(Animation::Idle);
        assert_eq!(player.until_next_frame(&atlas), Duration::from_millis(280));
        assert!(!player.advance(Duration::from_millis(279), &atlas));
        assert_eq!(player.until_next_frame(&atlas), Duration::from_millis(1));
        assert!(player.advance(Duration::from_millis(1), &atlas));
        assert_eq!(player.frame_index(), 1);
        assert_eq!(player.until_next_frame(&atlas), Duration::from_millis(110));
        assert!(player.advance(Duration::from_millis(820), &atlas));
        assert_eq!(player.frame_index(), 0);
        assert!(!player.advance(Duration::from_millis(1100), &atlas));
        assert!(!player.advance(Duration::ZERO, &atlas));
    }

    #[test]
    fn every_clip_observes_its_timings_and_completion_policy() {
        let atlas = HexAtlas::load().unwrap();
        for animation in Animation::ALL {
            let clip = atlas.clip(animation);
            let mut player = Player::new(animation);
            for (index, duration) in clip.frame_durations.iter().enumerate() {
                assert_eq!(player.frame_index(), index);
                assert!(!player.advance(*duration - Duration::from_nanos(1), &atlas));
                assert_eq!(player.until_next_frame(&atlas), Duration::from_nanos(1));
                assert!(player.advance(Duration::from_nanos(1), &atlas));
            }
            assert_eq!(player.frame_index(), 0);
            assert_eq!(
                player.animation(),
                if clip.looping {
                    animation
                } else {
                    Animation::Idle
                }
            );
        }
    }

    #[test]
    fn one_shot_carries_leftover_time_into_idle() {
        let atlas = HexAtlas::load().unwrap();
        for animation in [Animation::Waving, Animation::Succeeded, Animation::Failed] {
            let total: Duration = atlas.clip(animation).frame_durations.iter().copied().sum();
            let mut player = Player::new(animation);
            assert!(player.advance(total + Duration::from_millis(300), &atlas));
            assert_eq!(player.animation(), Animation::Idle);
            assert_eq!(player.frame_index(), 1);
            assert_eq!(player.until_next_frame(&atlas), Duration::from_millis(90));
        }
    }

    #[test]
    fn selection_resets_even_the_same_animation() {
        let atlas = HexAtlas::load().unwrap();
        let mut player = Player::new(Animation::Working);
        player.advance(Duration::from_millis(150), &atlas);
        player.select(Animation::Working);
        assert_eq!(player.frame_index(), 0);
        assert_eq!(player.until_next_frame(&atlas), Duration::from_millis(120));
        player.select(Animation::Waving);
        assert_eq!(player.animation(), Animation::Waving);
        assert_eq!(player.until_next_frame(&atlas), Duration::from_millis(140));
    }

    #[test]
    fn maximum_delta_matches_reduced_time() {
        let atlas = HexAtlas::load().unwrap();
        let idle_total: Duration = atlas
            .clip(Animation::Idle)
            .frame_durations
            .iter()
            .copied()
            .sum();
        for animation in Animation::ALL {
            let clip = atlas.clip(animation);
            let total: Duration = clip.frame_durations.iter().copied().sum();
            let mut actual = Player::new(animation);
            actual.advance(Duration::MAX, &atlas);
            let (expected_animation, nanos) = if clip.looping {
                (animation, Duration::MAX.as_nanos() % total.as_nanos())
            } else {
                (
                    Animation::Idle,
                    (Duration::MAX.as_nanos() - total.as_nanos()) % idle_total.as_nanos(),
                )
            };
            let mut expected = Player::new(expected_animation);
            expected.advance(Duration::from_nanos(nanos as u64), &atlas);
            assert_eq!(actual.animation(), expected.animation());
            assert_eq!(actual.frame_index(), expected.frame_index());
            assert_eq!(
                actual.until_next_frame(&atlas),
                expected.until_next_frame(&atlas)
            );
        }
    }
}
