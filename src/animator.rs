pub(crate) mod cache;
mod iterator;

use std::{sync::Arc, time::Duration};

use bevy::prelude::*;

#[cfg(feature = "custom_cursor")]
use bevy::window::{CursorIcon, CustomCursor, CustomCursorImage};
use bevy::{
    asset::{AssetId, Assets, Handle},
    ecs::{
        entity::Entity,
        message::MessageWriter,
        query::QueryData,
        resource::Resource,
        system::{Query, ResMut},
    },
    platform::collections::HashMap,
    reflect::Reflect,
    sprite::Sprite,
    time::Time,
    ui::widget::ImageNode,
};

#[cfg(feature = "3d")]
use crate::components::sprite3d::Sprite3d;
use crate::{
    animation::Animation,
    animator::{
        cache::AnimationCache,
        iterator::{AnimationIterator, IteratorFrame},
    },
    components::spritesheet_animation::{AnimationProgress, SpritesheetAnimation},
    events::AnimationEvent,
};
use iterator::AnimationIteratorEvent;

#[derive(Debug, Reflect)]
#[reflect(Debug)]
/// An instance of an animation that is currently being played
struct AnimationInstance {
    animation: Handle<Animation>,
    iterator: AnimationIterator,

    /// Current frame
    current_frame: Option<(IteratorFrame, AnimationProgress)>,

    /// Time accumulated since the last frame
    accumulated_time: Duration,
}

/// The animator is responsible for playing animations as time advances.
#[derive(Resource, Debug, Default, Reflect)]
#[reflect(Resource, Debug, Default)]
pub(crate) struct Animator {
    /// Animation caches, one for each animation
    ///
    /// They contain all the data required to play an animation.
    animation_caches: HashMap<AssetId<Animation>, Arc<AnimationCache>>,

    /// Instances of animations currently being played
    ///
    /// Each animation instance is associated to an entity with a [SpritesheetAnimation] component.
    animation_instances: HashMap<Entity, AnimationInstance>,
}

/// A query data type for the [`Animator::update`] system.
#[derive(QueryData)]
#[query_data(mutable, derive(Debug))]
pub(crate) struct SpritesheetAnimationQuery {
    entity: Entity,
    spritesheet_animation: &'static mut SpritesheetAnimation,
    sprite: Option<&'static mut Sprite>,
    #[cfg(feature = "3d")]
    sprite3d: Option<&'static mut Sprite3d>,
    image_node: Option<&'static mut ImageNode>,
    #[cfg(feature = "custom_cursor")]
    cursor_icon: Option<&'static mut CursorIcon>,
}

impl Animator {
    /// Advances the animations state
    pub fn animate(
        &mut self,
        time: &Time,
        message_writer: &mut MessageWriter<AnimationEvent>,
        query: &mut Query<SpritesheetAnimationQuery>,
        animations: &mut ResMut<Assets<Animation>>,
    ) {
        self.cleanup_instances(query);

        for mut item in query.iter_mut() {
            self.ensure_instance(&mut item, animations, message_writer);
            self.animate_entity(&mut item, time, message_writer);
        }
    }

    /// Syncs the sprites with the animation state
    pub fn sync_sprites(
        &mut self,
        message_writer: &mut MessageWriter<AnimationEvent>,
        query: &mut Query<SpritesheetAnimationQuery>,
        animations: &mut ResMut<Assets<Animation>>,
    ) {
        self.cleanup_instances(query);

        for mut item in query.iter_mut() {
            // Ensure instance exists (this handles the case where user swapped the animation component)
            self.ensure_instance(&mut item, animations, message_writer);

            // Apply the current frame to the sprite
            if let Some(instance) = self.animation_instances.get(&item.entity) {
                if let Some((frame, _)) = &instance.current_frame {
                    Self::apply_frame_to_sprite(&mut item, frame);
                }
            }
        }
    }

    fn cleanup_instances(&mut self, query: &Query<SpritesheetAnimationQuery>) {
        // Clear outdated animation instances associated to entities that do not have the component anymore
        self.animation_instances
            .retain(|entity, _state| query.contains(*entity));
    }

    fn animate_entity(
        &mut self,
        item: &mut SpritesheetAnimationQueryItem<'_, '_>,
        time: &Time,
        message_writer: &mut MessageWriter<AnimationEvent>,
    ) {
        let animation_instance = self.animation_instances.get_mut(&item.entity).unwrap();

        // Apply manual progress updates

        if animation_instance
            .current_frame
            .as_ref()
            .filter(|frame| item.spritesheet_animation.progress != frame.1)
            .is_some()
        {
            if animation_instance
                .iterator
                .to(item.spritesheet_animation.progress)
            {
                Self::next_frame(
                    &mut animation_instance.iterator,
                    item,
                    message_writer,
                )
                .inspect(|new_frame| {
                    animation_instance.current_frame = Some(new_frame.clone());
                    animation_instance.accumulated_time = Duration::ZERO;
                });
            } else {
                // Restore to the last valid progress if invalid
                item.spritesheet_animation.progress = animation_instance
                    .current_frame
                    .as_ref()
                    .map(|(_, progress)| *progress)
                    .unwrap_or_default()
            }
        }

        // Skip the update if the animation is paused
        //
        // (skipped AFTER the setup above so that the first frame is assigned, even if paused)

        if !item.spritesheet_animation.playing {
            return;
        }

        // Update the animation

        animation_instance.accumulated_time += Duration::from_secs_f32(
            time.delta_secs() * item.spritesheet_animation.speed_factor,
        );

        while let Some(current_frame) = animation_instance
            .current_frame
            .as_ref()
            .filter(|frame| animation_instance.accumulated_time > frame.0.duration)
        {
            // Consume the elapsed time

            animation_instance.accumulated_time -= current_frame.0.duration;

            // Fetch the next frame

            animation_instance.current_frame = Self::next_frame(
                &mut animation_instance.iterator,
                item,
                message_writer,
            )
            .or_else(|| {
                // The animation is over

                // Emit the end events if the animation just ended

                message_writer.write(AnimationEvent::ClipRepetitionEnd {
                    entity: item.entity,
                    clip_id: current_frame.0.clip_id,
                    clip_repetition: current_frame.0.clip_repetition,
                    animation: animation_instance.animation.clone(),
                });

                message_writer.write(AnimationEvent::ClipEnd {
                    entity: item.entity,
                    clip_id: current_frame.0.clip_id,
                    animation: animation_instance.animation.clone(),
                });

                message_writer.write(AnimationEvent::AnimationRepetitionEnd {
                    entity: item.entity,
                    animation: animation_instance.animation.clone(),
                    animation_repetition: current_frame.0.animation_repetition,
                });

                message_writer.write(AnimationEvent::AnimationEnd {
                    entity: item.entity,
                    animation: animation_instance.animation.clone(),
                });

                None
            });
        }
    }

    fn ensure_instance<'a>(
        &mut self,
        item: &mut SpritesheetAnimationQueryItem<'a, '_>,
        animations: &ResMut<Assets<Animation>>,
        message_writer: &mut MessageWriter<AnimationEvent>,
    ) {
        // Create a cache for the current animation if there are none yet

        let cache = self
            .animation_caches
            .entry(item.spritesheet_animation.animation.id())
            .or_insert_with(|| {
                let animation = animations
                    .get(item.spritesheet_animation.animation.id())
                    .unwrap();
                Arc::new(AnimationCache::from_animation(animation))
            });

        // Create a new animation instance if:
        // 1. The entity has an animation instance but it switched animation
        // 2. The entity has no animation instance yet
        let needs_new_animation_instance = self
            .animation_instances
            .get(&item.entity)
            .map(|instance| {
                instance.animation != item.spritesheet_animation.animation
                    || instance.current_frame.is_none()
                        && item.spritesheet_animation.progress.frame == 0
            })
            .unwrap_or(true);

        if needs_new_animation_instance {
            // Create a new iterator for this animation

            let mut iterator = AnimationIterator::new(cache.clone());

            // Move to the starting progress if specified

            if item.spritesheet_animation.progress != AnimationProgress::default() {
                // Start from the beginning if the progress is invalid
                if !iterator.to(item.spritesheet_animation.progress) {
                    item.spritesheet_animation.progress = AnimationProgress::default();
                }
            }

            // Create the instance and immediately play the first frame

            let first_frame = Self::next_frame(&mut iterator, item, message_writer);

            self.animation_instances.insert(
                item.entity,
                AnimationInstance {
                    animation: item.spritesheet_animation.animation.clone(),
                    iterator,
                    current_frame: first_frame,
                    accumulated_time: Duration::ZERO,
                },
            );
        }
    }

    fn next_frame(
        iterator: &mut AnimationIterator,
        item: &mut SpritesheetAnimationQueryItem<'_, '_>,
        message_writer: &mut MessageWriter<AnimationEvent>,
    ) -> Option<(IteratorFrame, AnimationProgress)> {
        let maybe_frame = iterator.next();

        if let Some((frame, progress)) = &maybe_frame {
            item.spritesheet_animation.progress = *progress;

            // Emit events

            Animator::emit_events(
                &frame.events,
                &item.spritesheet_animation.animation,
                &item.entity,
                message_writer,
            );
        }

        maybe_frame
    }

    fn apply_frame_to_sprite(
        item: &mut SpritesheetAnimationQueryItem<'_, '_>,
        frame: &IteratorFrame,
    ) {
        // Update the sprite
        // (we compare the indices to prevent needless "Changed" events)

        if let Some(atlas) = item
            .sprite
            .as_deref_mut()
            .and_then(|sprite| sprite.texture_atlas.as_mut())
            && atlas.index != frame.atlas_index
        {
            atlas.index = frame.atlas_index;
        }

        // 3D sprites

        #[cfg(feature = "3d")]
        if let Some(atlas) = item
            .sprite3d
            .as_deref_mut()
            .and_then(|sprite| sprite.texture_atlas.as_mut())
            && atlas.index != frame.atlas_index
        {
            atlas.index = frame.atlas_index;
        }

        // UI images

        if let Some(atlas) = item
            .image_node
            .as_deref_mut()
            .and_then(|image| image.texture_atlas.as_mut())
            && atlas.index != frame.atlas_index
        {
            atlas.index = frame.atlas_index;
        }

        // Cursors

        #[cfg(feature = "custom_cursor")]
        if let Some(atlas) = item
            .cursor_icon
            .as_deref_mut()
            .and_then(|cursor_icon| {
                if let CursorIcon::Custom(CustomCursor::Image(CustomCursorImage {
                    ref mut texture_atlas,
                    ..
                })) = *cursor_icon
                {
                    Some(texture_atlas)
                } else {
                    None
                }
            })
            .and_then(|atlas| atlas.as_mut())
            && atlas.index != frame.atlas_index
        {
            atlas.index = frame.atlas_index;
        }
    }

    fn emit_events(
        animation_events: &[AnimationIteratorEvent],
        animation: &Handle<Animation>,
        entity: &Entity,
        message_writer: &mut MessageWriter<AnimationEvent>,
    ) {
        animation_events.iter().for_each(|event| {
            message_writer.write(
                // Promote AnimationIteratorEvents to regular AnimationEvents
                match event {
                    AnimationIteratorEvent::MarkerHit {
                        marker,
                        clip_id,
                        clip_repetition,
                        animation_repetition,
                    } => AnimationEvent::MarkerHit {
                        entity: *entity,
                        marker: *marker,
                        clip_id: *clip_id,
                        clip_repetition: *clip_repetition,
                        animation: animation.clone(),
                        animation_repetition: *animation_repetition,
                    },
                    AnimationIteratorEvent::ClipRepetitionEnd {
                        clip_id,
                        clip_repetition,
                    } => AnimationEvent::ClipRepetitionEnd {
                        entity: *entity,
                        clip_id: *clip_id,
                        clip_repetition: *clip_repetition,
                        animation: animation.clone(),
                    },
                    AnimationIteratorEvent::ClipEnd { clip_id } => AnimationEvent::ClipEnd {
                        entity: *entity,
                        clip_id: *clip_id,
                        animation: animation.clone(),
                    },
                    AnimationIteratorEvent::AnimationRepetitionEnd {
                        animation_repetition,
                    } => AnimationEvent::AnimationRepetitionEnd {
                        entity: *entity,
                        animation: animation.clone(),
                        animation_repetition: *animation_repetition,
                    },
                },
            );
        });
    }
}
