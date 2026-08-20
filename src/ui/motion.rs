//! Le lissage de la molette.
//!
//! Un cran de molette est un **saut** : gpui traduit un `ScrollDelta::Lines`
//! en trois hauteurs de ligne et les ajoute d'un coup au décalage. Sur une
//! liste de texte — un diff, un arbre de quarante mille fichiers — l'œil perd
//! sa place à chaque cran, et il en faut une vingtaine pour traverser un hunk.
//! Ce module rejoue ce saut en une transition de cent soixante millisecondes,
//! amortie en fin de course.
//!
//! Le principe vient d'Aviary (`src/ui/motion.rs`), et il tient en une
//! inversion : **on n'empêche pas gpui de sauter**, il n'y a pas de phase de
//! capture pour la molette. On le laisse faire, on lit où il a atterri, on
//! **remet** le décalage d'avant, et on y va progressivement. D'où la place de
//! l'écouteur : sur un ancêtre **non défilant** de l'élément qui défile, donc
//! après son gestionnaire interne dans la phase de remontée.
//!
//! Quatre choses qu'il ne faut pas rater :
//!
//! - **Le saut se lit, il ne se recalcule pas.** gpui capture
//!   `window.line_height()` sous le style de texte de l'élément qui défile ;
//!   notre écouteur, posé sur un ancêtre, lit la hauteur ambiante. Sur le
//!   diff, qui n'a ni la police ni la taille de l'interface, les deux ne sont
//!   pas la même — d'où `Axis::jump`, qui prend la différence avec le décalage
//!   écrit à la frame précédente.
//! - **Un pavé tactile n'est pas une molette.** Il envoie des
//!   `ScrollDelta::Pixels`, déjà continus et attachés au doigt : les lisser
//!   ajouterait un retard à un geste direct. Ils passent tels quels, et
//!   annulent la transition en cours.
//! - **Un saut demandé par le code gagne.** `scroll_to_item` — une flèche qui
//!   change de hunk, `reveal_file` — écrit le décalage sans rien nous dire.
//!   `advance` compare donc ce qu'il trouve à ce qu'il avait écrit : un écart
//!   veut dire que quelqu'un d'autre est passé, et la transition est
//!   abandonnée plutôt que de ramener la vue en arrière.
//! - **La liste change de taille pendant le mouvement.** Un diff qui arrive,
//!   un dossier qu'on déplie : la destination est reprise à chaque frame sur
//!   les bornes du moment, depuis la position visible, pour que le
//!   recadrage reste continu.
//!
//! Les deux axes sont indépendants : le diff défile aussi en largeur, et une
//! transition verticale ne doit pas figer un décalage horizontal.

use gpui::{point, px, Pixels, Point, ScrollDelta, ScrollHandle, ScrollWheelEvent, Window};
use std::time::{Duration, Instant};

/// En deçà, deux positions sont la même. Sert à ne pas relancer une
/// transition vers la destination qu'on vise déjà.
const EPSILON: f32 = 0.0001;
/// Écart au-delà duquel on considère que quelqu'un d'autre a écrit le
/// décalage. Un demi-pixel de plus que l'arrondi de gpui.
const SYNC_EPSILON_PX: f32 = 0.75;
/// En deçà, on se pose plutôt que d'animer un dernier demi-pixel.
const SNAP_PX: f32 = 0.5;
/// Assez court pour que la liste réponde au cran, assez long pour que l'œil
/// suive le texte au lieu de le retrouver.
const DURATION: Duration = Duration::from_millis(160);

/// Ce qui défile dans un panneau : un axe, ou les deux.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axes {
    /// Le cas courant. Une molette horizontale y est traitée comme
    /// verticale, exactement comme le fait gpui quand un seul axe déborde.
    Vertical,
    /// Le diff, dont les lignes ne sont jamais renvoyées à la ligne.
    Both,
}

/// Le lissage d'une poignée de défilement.
///
/// Une par panneau, et rangée à côté de la poignée qu'elle anime : avancer le
/// mouvement d'un panneau sur le décalage d'un autre le ferait sauter d'un
/// bout à l'autre.
pub struct ScrollMotion {
    x: Axis,
    y: Axis,
    axes: Axes,
}

impl ScrollMotion {
    pub fn new(axes: Axes) -> Self {
        Self {
            x: Axis::default(),
            y: Axis::default(),
            axes,
        }
    }

    /// Avance la transition d'une frame et écrit le décalage obtenu.
    ///
    /// À appeler une fois par rendu du panneau, avant ou après avoir bâti son
    /// contenu — l'élément ne lit le décalage qu'à la mise en page.
    pub fn advance(&mut self, handle: &ScrollHandle, window: &Window) {
        let now = Instant::now();
        let actual = handle.offset();
        let max = handle.max_offset();
        let (x, moving_x) = self.x.advance(f32::from(actual.x), f32::from(max.x), now);
        let (y, moving_y) = self.y.advance(f32::from(actual.y), f32::from(max.y), now);
        if x != f32::from(actual.x) || y != f32::from(actual.y) {
            handle.set_offset(point(px(x), px(y)));
        }
        if moving_x || moving_y {
            window.request_animation_frame();
        }
    }

    /// Reprend le saut que gpui vient d'appliquer et le rejoue en transition.
    ///
    /// Rend vrai quand la vue doit être notifiée pour peindre la première
    /// frame ; `advance` demande les suivantes.
    pub fn on_wheel(
        &mut self,
        handle: &ScrollHandle,
        event: &ScrollWheelEvent,
        window: &Window,
    ) -> bool {
        let delta = match event.delta {
            // Le doigt est déjà progressif : le lisser ajouterait un retard.
            ScrollDelta::Pixels(_) => {
                self.cancel();
                return false;
            }
            ScrollDelta::Lines(_) => event.delta.pixel_delta(window.line_height()),
        };
        let (dx, dy) = self.split(delta);
        if dx == 0. && dy == 0. {
            return false;
        }

        let now = Instant::now();
        let jumped = handle.offset();
        let max = handle.max_offset();
        // L'axe que ce cran ne touche pas garde la position que `advance` lui
        // a écrite : la relire ici, c'est la lui rendre inchangée.
        let x = if dx == 0. {
            f32::from(jumped.x)
        } else {
            self.x
                .on_wheel(f32::from(jumped.x), dx, f32::from(max.x), now)
        };
        let y = if dy == 0. {
            f32::from(jumped.y)
        } else {
            self.y
                .on_wheel(f32::from(jumped.y), dy, f32::from(max.y), now)
        };
        handle.set_offset(point(px(x), px(y)));
        true
    }

    /// Abandonne la transition en cours, avant un geste direct.
    pub fn cancel(&mut self) {
        self.x.cancel();
        self.y.cancel();
    }

    /// Répartit un delta sur les axes **comme le fait gpui**, sans quoi le
    /// lissage déplacerait la vue autrement que le saut qu'il remplace.
    ///
    /// Sur un panneau à un seul axe, une molette horizontale bascule sur le
    /// vertical ; sur un panneau à deux axes, seule la composante dominante
    /// passe (`allow_concurrent_scroll` est faux par défaut).
    fn split(&self, delta: Point<Pixels>) -> (f32, f32) {
        match self.axes {
            Axes::Vertical => {
                let dy = if delta.y == px(0.) { delta.x } else { delta.y };
                (0., f32::from(dy))
            }
            Axes::Both if delta.x.abs() > delta.y.abs() => (f32::from(delta.x), 0.),
            Axes::Both => (0., f32::from(delta.y)),
        }
    }
}

/// Un axe, et rien de gpui dedans : c'est ce qui le rend testable.
#[derive(Default)]
struct Axis {
    motion: Option<Motion>,
    /// Le dernier décalage écrit. Il sert à reconnaître un saut venu
    /// d'ailleurs, et à retrouver la position d'avant celui de gpui même
    /// quand il a été rogné sur un bord.
    last: Option<f32>,
}

impl Axis {
    /// Rend le décalage à écrire, et s'il faut une frame de plus.
    fn advance(&mut self, actual: f32, max: f32, now: Instant) -> (f32, bool) {
        let Some(mut motion) = self.motion.take() else {
            self.last = Some(actual);
            return (actual, false);
        };
        // Quelqu'un d'autre a écrit le décalage : il gagne.
        if self
            .last
            .is_some_and(|expected| (actual - expected).abs() > SYNC_EPSILON_PX)
        {
            self.last = Some(actual);
            return (actual, false);
        }

        let mut sample = motion.sample_at(now);
        let target = motion.target.clamp(-max, 0.);
        // La liste a changé de taille : on repart de la position visible vers
        // la destination recadrée, pour que le mouvement reste continu.
        if (target - motion.target).abs() > EPSILON {
            motion = Motion::between(sample.value.clamp(-max, 0.), target, now);
            sample = motion.sample_at(now);
        }

        if !sample.running || (target - sample.value).abs() <= SNAP_PX {
            self.last = Some(target);
            return (target, false);
        }
        let current = sample.value.clamp(-max, 0.);
        self.last = Some(current);
        self.motion = Some(motion);
        (current, true)
    }

    /// Rend le décalage d'avant le saut, et vise celui d'après.
    fn on_wheel(&mut self, jumped: f32, delta: f32, max: f32, now: Instant) -> f32 {
        let clamp = |value: f32| value.clamp(-max, 0.);
        let jump = self.jump(jumped, delta);
        let (current, target) = match self.motion.take() {
            // Un cran pendant une transition allonge la destination.
            Some(motion) => (motion.sample_at(now).value, motion.target + jump),
            // La position d'avant est celle que nous avions écrite, et le saut
            // celui que gpui vient d'y ajouter.
            None => (jumped - jump, jumped),
        };
        let current = clamp(current);
        let target = clamp(target);
        if (target - current).abs() <= SNAP_PX {
            self.last = Some(target);
            return target;
        }
        self.last = Some(current);
        self.motion = Some(Motion::between(current, target, now));
        current
    }

    /// Le saut que gpui vient d'appliquer, **lu** plutôt que recalculé.
    ///
    /// `delta` est ce que vaudrait ce cran pour nous, et il ne vaut pas ce
    /// qu'il a valu pour gpui : celui-ci capture `window.line_height()` sous
    /// le **style de texte de l'élément qui défile**, alors que notre écouteur
    /// est sur un ancêtre et lit la hauteur de ligne ambiante. Le diff n'ayant
    /// ni la police ni la taille de l'interface, trois lignes d'écart font
    /// deux ou trois pixels.
    ///
    /// Cet écart est invisible au milieu d'un saut de soixante pixels — il
    /// décale seulement la provenance de la transition. **Sur un bord, il est
    /// tout le mouvement** : la destination y est rognée à la position qu'on
    /// occupe déjà, la provenance ne l'est pas, et la vue reculait de trois
    /// pixels pour y revenir en cent soixante millisecondes, à chaque cran.
    ///
    /// La différence avec le décalage écrit à la frame précédente, elle, est
    /// exacte. On ne s'y fie qu'à condition qu'elle ressemble au saut attendu :
    /// un décalage écrit par quelqu'un d'autre depuis notre dernier rendu
    /// rendrait cette note fausse, et la vue s'y téléporterait.
    fn jump(&self, jumped: f32, delta: f32) -> f32 {
        let Some(last) = self.last else { return delta };
        let observed = jumped - last;
        let plausible = observed * delta > 0. && observed.abs() <= delta.abs() * 2.;
        if plausible {
            observed
        } else {
            delta
        }
    }

    fn cancel(&mut self) {
        self.motion = None;
        self.last = None;
    }
}

struct Motion {
    from: f32,
    target: f32,
    started_at: Instant,
}

struct Sample {
    value: f32,
    running: bool,
}

impl Motion {
    fn between(from: f32, target: f32, now: Instant) -> Self {
        Self {
            from,
            target,
            started_at: now,
        }
    }

    fn sample_at(&self, now: Instant) -> Sample {
        let elapsed = now.saturating_duration_since(self.started_at);
        if elapsed >= DURATION {
            return Sample {
                value: self.target,
                running: false,
            };
        }
        let progress = elapsed.as_secs_f32() / DURATION.as_secs_f32();
        Sample {
            value: self.from + (self.target - self.from) * ease_out_cubic(progress),
            running: true,
        }
    }
}

/// Départ immédiat, arrivée en douceur. C'est la courbe qui donne
/// l'impression que la liste répond au cran plutôt qu'à un minuteur.
fn ease_out_cubic(progress: f32) -> f32 {
    1. - (1. - progress.clamp(0., 1.)).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: f32 = 1000.;

    /// Le cœur du procédé : le saut de gpui est rendu, et la position visée
    /// est celle où il avait atterri.
    #[test]
    fn a_notch_gives_back_the_jump_and_aims_at_it() {
        let now = Instant::now();
        let mut axis = Axis::default();
        // gpui est passé de 0 à -60 avant que nous soyons appelés.
        let written = axis.on_wheel(-60., -60., MAX, now);
        assert_eq!(written, 0., "la vue reste où l'œil l'a laissée");

        let (middle, moving) = axis.advance(0., MAX, now + Duration::from_millis(80));
        assert!(moving, "la transition demande une frame de plus");
        assert!(middle < 0. && middle > -60.);

        let (end, moving) = axis.advance(middle, MAX, now + DURATION);
        assert_eq!(end, -60.);
        assert!(!moving);
    }

    #[test]
    fn a_second_notch_extends_the_destination() {
        let now = Instant::now();
        let mut axis = Axis::default();
        axis.on_wheel(-60., -60., MAX, now);
        let middle = now + Duration::from_millis(80);
        let (position, _) = axis.advance(0., MAX, middle);
        // gpui saute de nouveau, depuis la position que nous venons d'écrire.
        axis.on_wheel(position - 60., -60., MAX, middle);
        let (end, _) = axis.advance(position, MAX, middle + DURATION);
        assert_eq!(end, -120., "les deux crans s'additionnent");
    }

    /// `scroll_to_item` écrit le décalage sans rien nous dire ; le ramener à
    /// la position de la transition annulerait la flèche qu'on vient de
    /// presser.
    #[test]
    fn a_programmatic_jump_wins_over_a_running_transition() {
        let now = Instant::now();
        let mut axis = Axis::default();
        axis.on_wheel(-60., -60., MAX, now);
        let (position, moving) = axis.advance(-900., MAX, now + Duration::from_millis(40));
        assert_eq!(position, -900., "la position demandée est gardée");
        assert!(!moving);
        assert!(axis.motion.is_none());
    }

    /// Près d'un bord, gpui ne rogne rien : il ajoute son saut au décalage et
    /// c'est la mise en page qui le ramènera dans les bornes. La destination
    /// est donc rognée, la provenance non.
    #[test]
    fn a_jump_past_the_edge_stops_at_it_without_starting_from_it() {
        let now = Instant::now();
        // On est à dix pixels du bas, un cran en demande soixante.
        let mut axis = Axis {
            last: Some(-990.),
            ..Default::default()
        };
        let written = axis.on_wheel(-1050., -60., MAX, now);
        assert_eq!(written, -990.);
        let (end, _) = axis.advance(written, MAX, now + DURATION);
        assert_eq!(end, -MAX, "la destination est rognée, pas la provenance");
    }

    /// Le bord, et le cran de trop.
    ///
    /// La hauteur de ligne que nous lisons n'est pas celle que gpui a
    /// utilisée : trois lignes d'écart font deux ou trois pixels. Ils sont
    /// invisibles au milieu d'un saut de soixante, mais ici ils *sont* le
    /// mouvement — la vue reculait d'autant avant d'y revenir, à chaque cran.
    #[test]
    fn a_notch_against_the_edge_leaves_the_view_where_it_is() {
        let now = Instant::now();
        let mut axis = Axis {
            last: Some(-MAX),
            ..Default::default()
        };
        // gpui a sauté de soixante pixels ; notre hauteur de ligne en compte
        // soixante-trois.
        let written = axis.on_wheel(-MAX - 60., -63., MAX, now);
        assert_eq!(written, -MAX, "la vue ne bouge pas d'un pixel");
        assert!(axis.motion.is_none(), "il n'y a rien à animer");
    }

    /// Le pavé tactile est déjà continu, et un panneau à un seul axe reçoit
    /// une molette horizontale comme une molette verticale.
    #[test]
    fn a_wheel_is_split_the_way_gpui_splits_it() {
        let vertical = ScrollMotion::new(Axes::Vertical);
        assert_eq!(vertical.split(point(px(-30.), px(0.))), (0., -30.));
        assert_eq!(vertical.split(point(px(-30.), px(-60.))), (0., -60.));

        let both = ScrollMotion::new(Axes::Both);
        assert_eq!(both.split(point(px(-30.), px(0.))), (-30., 0.));
        assert_eq!(
            both.split(point(px(-30.), px(-60.))),
            (0., -60.),
            "seule la composante dominante passe"
        );
    }
}
