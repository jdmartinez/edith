use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Axis {
    Columns,
    Rows,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Panel {
    #[default]
    Empty,
    Pane(usize),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Panel>,
        second: Box<Panel>,
    },
}

impl Panel {
    pub fn contains(&self, pane: usize) -> bool {
        match self {
            Self::Pane(index) => *index == pane,
            Self::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
            Self::Empty => false,
        }
    }

    pub fn first_pane(&self) -> Option<usize> {
        match self {
            Self::Pane(index) => Some(*index),
            Self::Split { first, second, .. } => first.first_pane().or_else(|| second.first_pane()),
            Self::Empty => None,
        }
    }

    fn leaf(&mut self, target: Option<usize>) -> Option<&mut Self> {
        match self {
            Self::Pane(index) if Some(*index) == target => Some(self),
            Self::Empty if target.is_none() => Some(self),
            Self::Split { first, second, .. } => first.leaf(target).or_else(|| second.leaf(target)),
            _ => None,
        }
    }

    fn path_to(&self, pane: usize, path: &mut Vec<bool>) -> bool {
        match self {
            Self::Pane(index) => *index == pane,
            Self::Empty => false,
            Self::Split { first, second, .. } => {
                path.push(false);
                if first.path_to(pane, path) {
                    return true;
                }
                *path.last_mut().unwrap() = true;
                if second.path_to(pane, path) {
                    return true;
                }
                path.pop();
                false
            }
        }
    }

    fn remove_at(&mut self, path: &[bool]) {
        match (path.split_first(), self) {
            (None, node) => *node = Self::Empty,
            (Some((side, rest)), node @ Self::Split { .. }) => {
                let Self::Split { first, second, .. } = node else {
                    unreachable!()
                };
                let (removed, sibling) = if *side {
                    (second, first)
                } else {
                    (first, second)
                };
                if rest.is_empty() {
                    *node = std::mem::take(sibling);
                } else {
                    removed.remove_at(rest);
                }
            }
            _ => unreachable!("validated panel path"),
        }
    }

    fn at(&mut self, path: &[bool]) -> Option<&mut Self> {
        match (path.split_first(), self) {
            (None, node) => Some(node),
            (Some((side, rest)), Self::Split { first, second, .. }) => {
                if *side { second } else { first }.at(rest)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DesktopLayout {
    pub root: Panel,
    pub focused: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PanelAction {
    Split { pane: usize, axis: Axis },
    Show { pane: usize },
    Hide { pane: usize },
    Close { pane: usize },
    Dismiss { path: Vec<bool> },
    Resize { path: Vec<bool>, ratio: f32 },
}

impl DesktopLayout {
    pub fn assign(&mut self, pane: usize) {
        if !self.root.contains(pane) {
            if let Some(empty) = self.root.leaf(None) {
                *empty = Panel::Pane(pane);
            } else if let Some(current) = self.root.leaf(self.focused) {
                *current = Panel::Pane(pane);
            }
        }
        self.focused = Some(pane);
    }

    pub fn show(&mut self, pane: usize) -> Result<()> {
        if !self.root.contains(pane)
            && self.root.leaf(None).is_none()
            && let Some(current) = self.focused
        {
            self.split(current, Axis::Columns)?;
        }
        self.assign(pane);
        Ok(())
    }

    pub fn hide(&mut self, pane: usize) -> Result<()> {
        let mut path = Vec::new();
        ensure!(
            self.root.path_to(pane, &mut path),
            "El panel ya no está visible"
        );
        self.root.remove_at(&path);
        if self.focused == Some(pane) {
            self.focused = self.root.first_pane();
        }
        Ok(())
    }

    pub fn dismiss(&mut self, path: &[bool]) -> Result<()> {
        ensure!(
            matches!(self.root.at(path), Some(Panel::Empty)),
            "El panel ya está ocupado"
        );
        self.root.remove_at(path);
        Ok(())
    }

    pub fn split(&mut self, pane: usize, axis: Axis) -> Result<()> {
        let Some(leaf) = self.root.leaf(Some(pane)) else {
            bail!("El panel ya no está visible");
        };
        *leaf = Panel::Split {
            axis,
            ratio: 0.5,
            first: Box::new(Panel::Pane(pane)),
            second: Box::new(Panel::Empty),
        };
        self.focused = Some(pane);
        Ok(())
    }

    pub fn resize(&mut self, path: &[bool], ratio: f32) -> Result<()> {
        ensure!(ratio.is_finite(), "Tamaño de panel inválido");
        let Some(Panel::Split { ratio: current, .. }) = self.root.at(path) else {
            bail!("El separador ya no está visible");
        };
        *current = ratio.clamp(0.15, 0.85);
        Ok(())
    }
}
