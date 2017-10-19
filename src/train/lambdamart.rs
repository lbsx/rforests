use train::regression_tree::*;
use train::dataset::*;
use util::*;
use metric::*;

pub struct LambdaMART<M> {
    dataset: DataSet,
    config: Config<M>,
}

pub struct Config<M> {
    pub trees: usize,
    pub learning_rate: f64,
    pub max_leaves: usize,
    pub min_samples_per_leaf: usize,
    pub thresholds: usize,
    pub print_metric: bool,
    pub print_tree: bool,
    pub metric: M,
    pub validation: Option<DataSet>,
}

impl<M> LambdaMART<M>
where
    M: MetricScorer,
{
    pub fn new(dataset: DataSet, config: Config<M>) -> LambdaMART<M> {
        LambdaMART {
            dataset: dataset,
            config: config,
        }
    }

    pub fn init(&self) -> Result<()> {
        Ok(())
    }

    pub fn learn(&self) -> Result<()> {
        let learning_rate = 0.1;
        let max_leaves = 10;
        let mut ensemble = Ensemble::new();
        let mut training = TrainingSet::from(&self.dataset);
        if self.config.print_metric {
            println!(
                "{:<7} | {:>9} | {:>9}",
                "#iter",
                self.config.metric.name() + "-T",
                self.config.metric.name() + "-V"
            );
        }
        for i in 0..self.config.trees {
            training.update_lambdas_weights();

            let mut tree = RegressionTree::new(
                learning_rate,
                max_leaves,
                self.config.min_samples_per_leaf,
            );

            // The scores of the model are updated when the tree node
            // does not split and becomes a leaf.
            tree.fit(&training);

            if self.config.print_tree {
                tree.print();
            }

            ensemble.push(tree);

            if self.config.print_metric {
                let train_score = training.evaluate(&self.config.metric);
                let mut validation_score = None;
                if let Some(ref validation) = self.config.validation {
                    validation_score = Some(validation.validate(
                        &ensemble,
                        &self.config.metric,
                    ));
                }

                if let Some(validation_score) = validation_score {
                    println!(
                        "{:<7} | {:>9.4} | {:>9.4}",
                        i,
                        train_score,
                        validation_score
                    );
                } else {
                    println!("{:<7} | {:>9.4} | {:>9.4}", i, train_score, "");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fs::File;

    #[test]
    fn test_lambda_mart() {
        let path = "/home/lbs/code/rforests/data/train-lite.txt";
        let max_bins = 256;
        let f = File::open(path).unwrap();
        let mut dataset = DataSet::new(max_bins);
        dataset.load(f).unwrap();

        let config = Config {
            trees: 1,
            learning_rate: 0.1,
            max_leaves: 10,
            min_samples_per_leaf: 1,
            thresholds: 256,
            print_metric: true,
            print_tree: false,
            metric: NDCGScorer::new(10),
            validation: None,
        };
        let lambdamart = LambdaMART::new(dataset, config);
        lambdamart.init().unwrap();
        lambdamart.learn().unwrap();
    }
}
