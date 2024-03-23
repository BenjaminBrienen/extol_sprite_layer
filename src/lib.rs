#![doc = include_str!("../README.md")]
use std::cmp::Reverse;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;

use bevy::prelude::*;
use ordered_float::OrderedFloat;
#[cfg(feature = "parallel_y_sort")]
use rayon::slice::ParallelSliceMut;

/// This plugin adjusts your entities' transforms so that their z-coordinates are sorted in the
/// proper order, where the order is specified by the `Layer` component. Note that since this sets
/// the z-coordinate, the children of a component with a sprite layer will effectively be on the
/// same sprite layer (though you can override this by giving them a sprite layer of their own). See
/// the crate documentation for how to use it.
///
/// The z-coordinate each entity will have is stored on [`RenderZCoordinate`] (including for
/// entities that don't have layers but are children of entities with layers); you shouldn't modify
/// this yourself, but you can read it if you need that information for something.
///
/// In general you should only instantiate this plugin with a single type you use throughout your
/// program.
///
/// By default your sprites will also be y-sorted. If you don't need this, replace the
/// [`SpriteLayerOptions`] like so:
///
/// ```
/// # use bevy::prelude::*;
/// # use extol_sprite_layer::SpriteLayerOptions;
/// # let mut app = App::new();
/// app.insert_resource(SpriteLayerOptions { y_sort: false });
/// ```
pub struct SpriteLayerPlugin<Layer> {
    phantom: PhantomData<Layer>,
}

impl<Layer> Default for SpriteLayerPlugin<Layer> {
    fn default() -> Self {
        Self {
            phantom: Default::default(),
        }
    }
}

impl<Layer: LayerIndex> Plugin for SpriteLayerPlugin<Layer> {
    fn build(&self, app: &mut App) {
        app.init_resource::<SpriteLayerOptions>()
            .add_systems(
                First,
                clear_z_coordinates.in_set(SpriteLayerSet::ClearZCoordinates),
            )
            .add_systems(
                Last,
                // We need to run these systems *after* the transform's systems because they need the
                // proper y-coordinate to be set for y-sorting.
                (
                    inherited_layers::<Layer>.pipe(compute_render_z_coordinates::<Layer>),
                    update_global_transforms,
                )
                    .chain()
                    .in_set(SpriteLayerSet::SetZCoordinates),
            )
            .register_type::<RenderZCoordinate>();
    }
}

/// Configure how the sprite layer
#[derive(Debug, Resource, Reflect)]
pub struct SpriteLayerOptions {
    pub y_sort: bool,
}

impl Default for SpriteLayerOptions {
    fn default() -> Self {
        Self { y_sort: true }
    }
}

/// Set for all systems related to [`SpriteLayerPlugin`]. This is run in the
/// render app's [`ExtractSchedule`], *not* the main app.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, SystemSet)]
pub enum SpriteLayerSet {
    ClearZCoordinates,
    SetZCoordinates,
}

/// Trait for the type you use to indicate your sprites' layers. Add this as a
/// component to any entity you want to treat as a sprite. Note that this does
/// *not* propagate.
pub trait LayerIndex: Eq + Hash + Component + Clone + Debug {
    /// The actual numeric z-value that the layer index corresponds to.  Note
    /// that the z-value for an entity can be any value in the range
    /// `layer.as_z_coordinate() <= z < layer.as_z_coordinate() + 1.0`, and the
    /// exact values are an implementation detail!
    ///
    /// With the default Bevy camera settings, your return values from this
    /// function should be between 0 and 999.0, since the camera is at z =
    /// 1000.0. Prefer smaller z-values since that gives more precision.
    fn as_z_coordinate(&self) -> f32;
}

/// Clears the z-coordinate of everything with a `RenderZCoordinate` component.
pub fn clear_z_coordinates(mut query: Query<&mut Transform, With<RenderZCoordinate>>) {
    for mut transform in query.iter_mut() {
        transform.bypass_change_detection().translation.z = 0.0;
    }
}

/// Propagates the `Layer` of each entity to the `InheritedLayer` of itself and all of its
/// descendants.
pub fn inherited_layers<Layer: LayerIndex>(
    recursive_query: Query<(Option<&Children>, Option<&Layer>)>,
    root_query: Query<(Entity, &Layer), Without<Parent>>,
) -> HashMap<Entity, Layer> {
    let mut layer_map = HashMap::new();
    for (entity, layer) in &root_query {
        propagate_layers_impl(entity, layer, &recursive_query, &mut layer_map);
    }
    layer_map
}

/// Recursive impl for [`inherited_layers`].
fn propagate_layers_impl<Layer: LayerIndex>(
    entity: Entity,
    propagated_layer: &Layer,
    query: &Query<(Option<&Children>, Option<&Layer>)>,
    layer_map: &mut HashMap<Entity, Layer>,
) {
    let (children, layer) = query.get(entity).expect("query shouldn't ever fail");
    let layer = layer.unwrap_or(propagated_layer);
    layer_map.insert(entity, layer.clone());

    let Some(children) = children else {
        return;
    };

    for child in children {
        propagate_layers_impl(*child, layer, query, layer_map);
    }
}

/// Compute the z-coordinate that each entity should have. This is equal to its layer's equivalent
/// z-coordinate, plus an offset in the range [0, 1) corresponding to its y-sorted position
/// (if y-sorting is enabled).
pub fn compute_render_z_coordinates<Layer: LayerIndex>(
    In(layers): In<HashMap<Entity, Layer>>,
    mut commands: Commands,
    query: Query<&GlobalTransform>,
    options: Res<SpriteLayerOptions>,
) {
    if options.y_sort {
        // We y-sort everything because this avoids the overhead of grouping
        // entities by their layer. Using sort_by_cached_key to make the vec's
        // elements smaller doesn't seem to help here.
        let mut sort_keys: Vec<(ZIndexSortKey, Entity)> = layers
            .keys()
            .map(|entity| {
                (
                    query
                        .get(*entity)
                        .map(ZIndexSortKey::new)
                        .unwrap_or_else(|_| ZIndexSortKey::new(&Default::default())),
                    *entity,
                )
            })
            .collect();

        // most of the expense is here.
        #[cfg(feature = "parallel_y_sort")]
        sort_keys.par_sort_unstable();
        #[cfg(not(feature = "parallel_y_sort"))]
        sort_keys.sort_unstable();

        let scale_factor = 1.0 / sort_keys.len() as f32;
        for (i, (_, entity)) in sort_keys.into_iter().enumerate() {
            let z = layers[&entity].as_z_coordinate() + (i as f32) * scale_factor;
            commands.entity(entity).try_insert(RenderZCoordinate(z));
        }
    } else {
        for (entity, layer) in &layers {
            commands
                .entity(*entity)
                .try_insert(RenderZCoordinate(layer.as_z_coordinate()));
        }
    }
}

/// Sets the z-coordinate of each entity's [`GlobalTransform`] from its [`RenderZCoordinate`]
pub fn update_global_transforms(mut query: Query<(&RenderZCoordinate, &mut GlobalTransform)>) {
    for (render_coordinate, mut transform) in query.iter_mut() {
        // hacky hacky; I can't find a way to directly mutate the GlobalTransform.
        let mut affine = transform.affine();
        affine.translation.z = render_coordinate.get();
        *transform = GlobalTransform::from(affine);
    }
}

/// Used to sort the entities within a sprite layer.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ZIndexSortKey(Reverse<OrderedFloat<f32>>);

impl ZIndexSortKey {
    // This is reversed because bevy uses +y pointing upwards, which is the
    // opposite of what you generally want.
    fn new(transform: &GlobalTransform) -> Self {
        Self(Reverse(OrderedFloat(transform.translation().y)))
    }
}

/// Stores the z-coordinate that will be used at render time. Don't modify this yourself.
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd, Component, Reflect)]
pub struct RenderZCoordinate(pub f32);

// we use new/get instead of destructuring to make using it externally a little more annoying
// and for forwards compat
impl RenderZCoordinate {
    pub fn new(z: f32) -> Self {
        Self(z)
    }

    fn get(self) -> f32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Component)]
    enum Layer {
        Top,
        Middle,
        Bottom,
    }

    impl LayerIndex for Layer {
        fn as_z_coordinate(&self) -> f32 {
            use Layer::*;
            match self {
                Bottom => 0.0,
                Middle => 1.0,
                Top => 2.0,
            }
        }
    }

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(TransformPlugin)
            .add_plugins(SpriteLayerPlugin::<Layer>::default());

        app
    }

    /// Just verify that adding the plugin doesn't somehow blow everything up.
    #[test]
    fn plugin_add_smoke_check() {
        let _ = test_app();
    }

    fn transform_at(x: f32, y: f32) -> TransformBundle {
        TransformBundle::from_transform(Transform::from_xyz(x, y, 0.0))
    }

    #[test]
    fn simple() {
        let mut app = test_app();
        let top = app.world.spawn((transform_at(1.0, 1.0), Layer::Top)).id();
        let middle = app
            .world
            .spawn((transform_at(1.0, 1.0), Layer::Middle))
            .id();
        let bottom = app
            .world
            .spawn((transform_at(1.0, 1.0), Layer::Bottom))
            .id();
        app.update();

        let get_z = |entity| {
            let render_coordinate = app.world.get::<RenderZCoordinate>(entity).unwrap().0;
            let transform_z = app
                .world
                .get::<GlobalTransform>(entity)
                .unwrap()
                .translation()
                .z;
            assert_eq!(
                render_coordinate, transform_z,
                "inconsistent z-coordinate for {entity:?}"
            );
            transform_z
        };
        assert!(get_z(bottom) < get_z(middle));
        assert!(get_z(middle) < get_z(top));
    }
}
