mod context;

use bevy::prelude::*;
use bevy_spritesheet_animation::prelude::*;
use context::*;

#[derive(Resource)]
struct AnimHandles {
    #[allow(dead_code)]
    anim1: Handle<Animation>,
    anim2: Handle<Animation>,
}

fn switch_anim_system(
    mut commands: Commands,
    mut messages: MessageReader<AnimationEvent>,
    handles: Res<AnimHandles>,
) {
    for message in messages.read() {
        match message {
            AnimationEvent::AnimationRepetitionEnd { entity, .. }
            | AnimationEvent::ClipRepetitionEnd { entity, .. } => {
                commands
                    .entity(*entity)
                    .insert(SpritesheetAnimation::new(handles.anim2.clone()));
            }
            _ => {}
        }
    }
}

#[test]
fn switch_animation_on_repetition_end() {
    let mut ctx = Context::new();

    let anim1 = ctx.create_animation(|builder| {
        builder
            .set_duration(AnimationDuration::PerFrame(100))
            .set_repetitions(AnimationRepeat::Loop)
            .add_indices([0, 1])
    });

    let anim2 = ctx.create_animation(|builder| {
        builder
            .set_duration(AnimationDuration::PerFrame(100))
            .set_repetitions(AnimationRepeat::Loop)
            .add_indices([2, 3])
    });

    ctx.app.insert_resource(AnimHandles {
        anim1: anim1.clone(),
        anim2: anim2.clone(),
    });

    ctx.app.add_systems(Update, switch_anim_system);

    ctx.app
        .world_mut()
        .entity_mut(ctx.sprite_entity)
        .insert(SpritesheetAnimation::new(anim1.clone()));

    // 0ms
    ctx.run(50);
    ctx.check(0, []);

    // 100ms
    ctx.run(100);
    ctx.check(1, []);

    // 200ms
    // The animator runs. Updates sprite to 0. Emits event.
    // Our system runs. Receives event. Updates component to anim2.
    // Frame ends.
    ctx.run(100);

    // If bug: sprite is 0 (first frame of anim1).
    // If fixed: sprite is 2 (first frame of anim2).

    // Manual check for better logging
    let entity_ref = ctx.app.world().entity(ctx.sprite_entity);
    let atlas = entity_ref
        .get::<Sprite>()
        .and_then(|sprite| sprite.texture_atlas.as_ref())
        .unwrap();

    assert_eq!(
        atlas.index, 
        2, 
        "Expected index 2 (start of anim2), but found index {}. The animation momentarily reverted to the start of the old animation (anim1) before switching.", 
        atlas.index
    );
}
