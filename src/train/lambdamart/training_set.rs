use std::cell::Cell;
use metric::MetricScorer;
use super::histogram::*;
use util::{Id, Value};
use std;
use std::cmp::Ordering::*;
use train::dataset::*;

/// A Mapping from the index of a Instance in the DataSet into a
/// threshold interval.
struct ThresholdMap {
    /// Thresholds are ordered in ascending order. To test whether a
    /// value falls into the threshold, use `if value <= threshold`.
    thresholds: Vec<Value>,

    /// The index of the Vec is the index of the instances in the
    /// DataSet, which also means `map.len() == instances.len()`.
    ///
    /// The elements are the indices into the thresholds Vec.
    ///
    /// For example, if we have 100,000 instances, and 256 thresholds,
    /// then
    /// ```
    /// assert_eq!(map.len(), 100,000);
    /// assert!(map.iter().all(|&i| i <= 256));
    /// ```
    map: Vec<usize>,
}

impl ThresholdMap {
    /// Generate thresholds according to the given values and max
    /// bins. If the count of values exceeds max bins, thresholds are
    /// generated by averaging the difference of max and min of the
    /// values by max bins.
    fn thresholds(
        sorted_values: Vec<Value>,
        thresholds_count: usize,
    ) -> Vec<Value> {
        let mut thresholds = sorted_values;

        thresholds.dedup();

        // If too many values, generate at most thresholds_count thresholds.
        if thresholds.len() > thresholds_count {
            let max = *thresholds.last().unwrap();
            let min = *thresholds.first().unwrap();
            let step = (max - min) / thresholds_count as Value;
            thresholds = (0..thresholds_count)
                .map(|n| min + n as Value * step)
                .collect();
        }
        thresholds.push(std::f64::MAX);
        thresholds
    }

    /// Create a map according to the given values and max bins.
    pub fn new(values: Vec<Value>, thresholds_count: usize) -> ThresholdMap {
        let nvalues = values.len();

        let mut indexed_values: Vec<(usize, Value)> =
            values.iter().cloned().enumerate().collect();
        indexed_values.sort_by(|&(_, a), &(_, b)| {
            a.partial_cmp(&b).unwrap_or(Less)
        });

        let sorted_values = indexed_values
            .iter()
            .map(|&(_, value)| value)
            .collect::<Vec<Value>>();
        let thresholds =
            ThresholdMap::thresholds(sorted_values, thresholds_count);
        let mut map: Vec<usize> = Vec::new();
        map.resize(nvalues, 0);

        let mut value_pos = 0;
        for (threshold_index, &threshold) in thresholds.iter().enumerate() {
            for &(value_index, value) in indexed_values[value_pos..].iter() {
                if value > threshold {
                    break;
                }
                map[value_index] = threshold_index;
                value_pos += 1;
            }
        }
        ThresholdMap {
            thresholds: thresholds,
            map: map,
        }
    }

    /// Generate a histogram for a series of values.
    ///
    /// The input is an iterator over (instance id, feature value,
    /// label value).
    ///
    /// There are two cases when we need to regenerate the
    /// histogram. First, after each iteration of learning, the label
    /// values are different. But this is a situation that we can
    /// update the histogram instead of constructing from
    /// scratch. Second, after a tree node is splited, each sub-node
    /// contains different part of data.
    ///
    /// # Examples
    ///
    /// let data = vec![
    ///     // target value, feature values
    ///     (3.0, 5.0),
    ///     (2.0, 7.0),
    ///     (3.0, 3.0),
    ///     (1.0, 2.0),
    ///     (0.0, 1.0),
    ///     (2.0, 8.0),
    ///     (4.0, 9.0),
    ///     (1.0, 4.0),
    ///     (0.0, 6.0),
    /// ];
    ///
    /// let map = ThresholdMap::new(data.iter().map(|&(_, value)| value), 3);
    /// let histogram = map.histogram(data.iter().map(|&(target, _)| target));
    ///
    /// assert_eq!(histogram.variance(), 15.555555555555557);
    pub fn histogram<I: Iterator<Item = (Id, Value, Value)>>(
        &self,
        iter: I,
    ) -> Histogram {
        // (threshold value, count, sum, squared_sum)
        let mut hist: Vec<(Value, usize, Value, Value)> = self.thresholds
            .iter()
            .map(|&threshold| (threshold, 0, 0.0, 0.0))
            .collect();

        for (id, feature_value, label) in iter {
            let threshold_index = self.map[id];

            let threshold = self.thresholds[threshold_index];
            assert!(feature_value <= threshold);

            hist[threshold_index].1 += 1;
            hist[threshold_index].2 += label;
            hist[threshold_index].3 += label * label;
        }

        for i in 1..hist.len() {
            hist[i].1 += hist[i - 1].1;
            hist[i].2 += hist[i - 1].2;
            hist[i].3 += hist[i - 1].3;
        }
        let feature_histogram = hist.into_iter().collect();
        feature_histogram
    }
}

impl std::fmt::Debug for ThresholdMap {
    // Avoid printing the very long f64::MAX value.
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "ThresholdMap {{ thresholds: {:?}, map: {:?} }}",
            self.thresholds
                .iter()
                .map(|&threshold| if threshold == std::f64::MAX {
                    "Value::MAX".to_string()
                } else {
                    threshold.to_string()
                })
                .collect::<Vec<String>>()
                .join(", "),
            self.map
        )
    }
}

/// A collection type containing a data set. The difference with
/// DataSet is that this data structure keeps the latest label values
/// after each training.
pub struct TrainingSet<'d> {
    dataset: &'d DataSet,
    // Fitting result of the model. We need to update the result at
    // each leaf node.
    model_scores: Vec<Cell<Value>>,
    // Gradients, or lambdas.
    lambdas: Vec<Value>,
    // Newton step weights
    weights: Vec<Value>,
    threshold_maps: Vec<ThresholdMap>,
}

impl<'d> TrainingSet<'d> {
    /// Creates a new TrainingSet from DataSet. Thresholds will be
    /// generated.
    pub fn new(
        dataset: &'d DataSet,
        thresholds_count: usize,
    ) -> TrainingSet<'d> {
        fn generate_thresholds(
            dataset: &DataSet,
            thresholds_count: usize,
        ) -> Vec<ThresholdMap> {
            let mut threshold_maps = Vec::new();
            for fid in dataset.fid_iter() {
                let values: Vec<Value> =
                    dataset.feature_value_iter(fid).collect();
                let map = ThresholdMap::new(values, thresholds_count);

                threshold_maps.push(map);
            }
            threshold_maps
        }

        let len = dataset.len();

        let mut model_scores = Vec::with_capacity(len);
        model_scores.resize(len, Cell::new(0.0));

        let mut lambdas = Vec::with_capacity(len);
        lambdas.resize(len, 0.0);

        let mut weights = Vec::with_capacity(len);
        weights.resize(len, 0.0);

        TrainingSet {
            dataset: dataset,
            model_scores: model_scores,
            lambdas: lambdas,
            weights: weights,
            threshold_maps: generate_thresholds(dataset, thresholds_count),
        }
    }

    /// Returns the number of instances in the training set, also
    /// referred to as its 'length'.
    fn len(&self) -> usize {
        self.model_scores.len()
    }

    /// Get (label, instance) at given index.
    fn get(&self, index: usize) -> (Value, &'d Instance) {
        (self.model_scores[index].get(), &self.dataset[index])
    }

    /// Get lambda at given index.
    fn lambda(&self, index: usize) -> Value {
        self.lambdas[index]
    }

    /// Get (lambda, weight) at given index.
    fn get_lambda_weight(&self, index: usize) -> (Value, Value) {
        (self.lambdas[index], self.weights[index])
    }

    /// Returns an iterator over the feature ids in the training set.
    pub fn fid_iter(&self) -> impl Iterator<Item = Id> {
        self.dataset.fid_iter()
    }

    pub fn init_model_scores(&mut self, values: &[Value]) {
        assert_eq!(self.len(), values.len());
        for (score, &value) in self.model_scores.iter_mut().zip(values.iter()) {
            score.set(value);
        }
    }

    /// Returns an iterator over the labels in the data set.
    pub fn iter(&'d self) -> impl Iterator<Item = (Value, &Instance)> + 'd {
        self.model_scores.iter().map(|celled| celled.get()).zip(
            self.dataset.iter(),
        )
    }

    /// Returns an iterator over the labels in the data set.
    pub fn model_score_iter(&'d self) -> impl Iterator<Item = Value> + 'd {
        self.model_scores.iter().map(|celled| celled.get())
    }

    /// Returns the label value at given index.
    pub fn model_score(&self, index: usize) -> f64 {
        self.model_scores[index].get()
    }

    /// Adds delta to each label specified in `indices`.
    pub fn update_result(&self, indices: &[Id], delta: Value) {
        assert!(indices.len() <= self.model_scores.len());
        for &index in indices.iter() {
            let celled_score = &self.model_scores[index];
            celled_score.set(celled_score.get() + delta);
        }
    }

    /// Generate histogram for the specified instances. The input
    /// iterator specifies the indices of instance that we want to
    /// generate histogram on. For a training data set, the histogram
    /// is used to make statistics of the lambda values, which is
    /// actually the target value that we aims to fit to in the
    /// current iteration of learning.
    fn feature_histogram<I: Iterator<Item = Id>>(
        &self,
        fid: Id,
        iter: I,
    ) -> Histogram {
        // Get the map by feature id.
        let iter = iter.map(|id| (id, self.lambdas[id]));

        // Get the map by feature id.
        let threshold_map = &self.threshold_maps[fid - 1];
        let iter =
            iter.map(|(id, target)| (id, self.dataset[id].value(fid), target));
        threshold_map.histogram(iter)
    }

    /// Updates the lambda and weight for each instance.
    ///
    /// 1. For each query, rank the instances by the scores of our
    /// model.
    ///
    /// 2. Compute the change of scores by swaping each instance with
    /// another
    ///
    /// 3. Update lambda and weight according to the formulas
    pub fn update_lambdas_weights<'a, 'b>(
        &'a mut self,
        metric: &Box<MetricScorer>,
    ) {
        for (l, w) in self.lambdas.iter_mut().zip(self.weights.iter_mut()) {
            *l = 0.0;
            *w = 0.0;
        }

        for (qid, mut query) in self.dataset.query_iter() {
            debug!("Update lambdas for qid {}", qid);
            use std::cmp::Ordering;

            let mut rank_list: Vec<_> = query
                .iter()
                .map(|&index| {
                    (
                        index,
                        self.dataset[index].label(),
                        self.model_scores[index].get(),
                    )
                })
                .collect();

            // Rank by the scores of our model.
            rank_list.sort_by(|&(_, _, score1), &(_, _, score2)| {
                score2.partial_cmp(&score1).unwrap_or(Ordering::Equal)
            });

            let ranked_labels: Vec<_> =
                rank_list.iter().map(|&(_, label, _)| label).collect();

            let metric_delta = metric.delta(&ranked_labels);

            let k = metric.get_k();
            for (metric_index1, &(index1, label1, score1)) in
                rank_list.iter().enumerate()
            {
                for (metric_index2, &(index2, label2, score2)) in
                    rank_list.iter().enumerate()
                {
                    if metric_index1 > k && metric_index2 > k {
                        break;
                    }

                    if label1 <= label2 {
                        continue;
                    }

                    let metric_delta_value =
                        metric_delta[metric_index1][metric_index2].abs();
                    let rho = 1.0 / (1.0 + (score1 - score2).exp());
                    let lambda = metric_delta_value * rho;
                    let weight = rho * (1.0 - rho) * metric_delta_value;

                    self.lambdas[index1] += lambda;
                    self.weights[index1] += weight;
                    self.lambdas[index2] -= lambda;
                    self.weights[index2] += weight;
                }
            }
        }
    }

    pub fn evaluate(&self, metric: &Box<MetricScorer>) -> f64 {
        let mut score = 0.0;
        let mut count = 0;
        for (_qid, mut indices) in self.dataset.query_iter() {
            // Sort the indices by the score of the model, rank the
            // query based on the scores, then measure the output.

            indices.sort_by(|&index1, &index2| {
                self.model_score(index2)
                    .partial_cmp(&self.model_score(index1))
                    .unwrap()
            });

            let labels: Vec<Value> = indices
                .iter()
                .map(|&index| self.dataset[index].label())
                .collect();

            count += 1;
            score += metric.score(&labels);
        }

        score / count as f64
    }
}

/// A collection type containing part of a data set.
pub struct TrainingSample<'t, 'd: 't> {
    /// Original data
    training: &'t TrainingSet<'d>,

    /// Indices into training
    indices: Vec<usize>,
}

impl<'t, 'd: 't> TrainingSample<'t, 'd> {
    /// Returns the number of instances in the data set sample, also
    /// referred to as its 'length'.
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    /// Creates an iterator which gives the index of the Instance as
    /// well as the Instance.
    ///
    /// The iterator returned yields pairs (index, value, instance),
    /// where `index` is the index of Instance, `value` is the label
    /// value, and `instance` is the reference to the Instance.
    pub fn iter<'a>(
        &'a self,
    ) -> impl Iterator<Item = (Id, Value, &Instance)> + 'a {
        self.indices.iter().map(move |&index| {
            let (label, instance) = self.training.get(index);
            (index, label, instance)
        })
    }

    /// Returns an iterator over the feature ids in the data set
    /// sample.
    pub fn fid_iter<'a>(&'a self) -> impl Iterator<Item = Id> + 'a {
        self.training.fid_iter()
    }

    /// Returns an iterator over the labels in the data set sample.
    pub fn label_iter<'a>(&'a self) -> impl Iterator<Item = Value> + 'a {
        self.iter().map(|(_index, label, _ins)| label)
    }

    /// Returns an iterator over the values of the given feature in
    /// the data set sample.
    pub fn value_iter<'a>(
        &'a self,
        fid: Id,
    ) -> impl Iterator<Item = Value> + 'a {
        self.iter().map(move |(_index, _label, ins)| ins.value(fid))
    }

    /// Returns the Newton step value.
    pub fn newton_output(&self) -> f64 {
        let (lambda_sum, weight_sum) = self.indices.iter().fold(
            (0.0, 0.0),
            |(lambda_sum,
              weight_sum),
             &index| {
                let (lambda, weight) = self.training.get_lambda_weight(index);
                (lambda_sum + lambda, weight_sum + weight)
            },
        );

        if weight_sum == 0.0 {
            0.0
        } else {
            lambda_sum / weight_sum
        }
    }

    pub fn update_output(&self, delta: Value) {
        self.training.update_result(&self.indices, delta);
    }

    /// Returns a histogram of the feature of the data set sample.
    fn feature_histogram(&self, fid: Id) -> Histogram {
        self.training.feature_histogram(
            fid,
            self.indices.iter().cloned(),
        )
    }

    /// To facilitate computing the variance. We made a little
    /// transformation.
    ///
    /// variance = sum((labels - label_avg) ^ 2), where label_avg =
    /// sum(labels) / count.
    ///
    /// Finally, the variance is computed using the formula:
    ///
    /// variance = sum(labels ^ 2) - sum(labels) ^ 2 / left_count
    pub fn variance(&self) -> f64 {
        let (sum, squared_sum) = self.indices.iter().fold(
            (0.0, 0.0),
            |(sum, squared_sum),
             &index| {
                let value = self.training.lambda(index);
                (sum + value, squared_sum + value * value)
            },
        );
        let count = self.indices.len() as f64;
        let variance = squared_sum - sum * sum / count;
        variance
    }

    /// Split self. Returns (split feature, threshold, s value, left
    /// child, right child). For each split, if its variance is zero,
    /// it's non-splitable.
    pub fn split(
        &self,
        min_leaf_count: usize,
    ) -> Option<(Id, Value, f64, TrainingSample<'t, 'd>, TrainingSample<'t, 'd>)> {
        assert!(min_leaf_count > 0);
        if self.variance().abs() <= 0.000001 {
            return None;
        }

        // (fid, threshold, s)
        let mut splits: Vec<(Id, Value, f64)> = Vec::new();
        for fid in self.fid_iter() {
            debug!("Find best split for fid {}", fid);
            let feature_histogram = self.feature_histogram(fid);
            let split = feature_histogram.best_split(min_leaf_count);
            match split {
                Some((threshold, s)) => splits.push((fid, threshold, s)),
                None => continue,
            }
        }

        // Find the split with the best s value;
        let (fid, threshold, s) = match splits.into_iter().max_by(|a, b| {
            a.2.partial_cmp(&b.2).unwrap()
        }) {
            Some((fid, threshold, s)) => (fid, threshold, s),
            None => return None,
        };

        let mut left_indices = Vec::new();
        let mut right_indices = Vec::new();
        for (index, _label, instance) in self.iter() {
            if instance.value(fid) <= threshold {
                left_indices.push(index);
            } else {
                right_indices.push(index);
            }
        }

        let left = TrainingSample {
            training: self.training,
            indices: left_indices,
        };
        let right = TrainingSample {
            training: self.training,
            indices: right_indices,
        };
        Some((fid, threshold, s, left, right))
    }
}

impl<'t, 'd> From<&'t TrainingSet<'d>> for TrainingSample<'t, 'd> {
    fn from(training: &'t TrainingSet<'d>) -> TrainingSample<'t, 'd> {
        let len = training.len();
        let indices: Vec<usize> = (0..len).collect();
        TrainingSample {
            training: training,
            indices: indices,
        }
    }
}

impl<'t, 'd> std::fmt::Display for TrainingSample<'t, 'd> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        for &index in self.indices.iter() {
            let (label, instance) = self.training.get(index);

            write!(
                f,
                "{{index: {}, label: {}, instance: {}}}\n",
                index,
                label,
                instance
            )?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test::Bencher;
    use metric;

    #[test]
    fn test_instance_interface() {
        let label = 3.0;
        let qid = 3333;
        let values = vec![1.0, 2.0, 3.0];
        let instance = Instance::new(label, qid, values);

        // value_iter()
        let mut iter = instance.value_iter();
        assert_eq!(iter.next(), Some((1, 1.0)));
        assert_eq!(iter.next(), Some((2, 2.0)));
        assert_eq!(iter.next(), Some((3, 3.0)));
        assert_eq!(iter.next(), None);

        // value()
        assert_eq!(instance.value(1), 1.0);
        assert_eq!(instance.value(2), 2.0);
        assert_eq!(instance.value(3), 3.0);
        assert_eq!(instance.value(4), 0.0);

        // max_feature_id()
        assert_eq!(instance.max_feature_id(), 3);

        // label()
        assert_eq!(instance.label(), 3.0);

        // qid()
        assert_eq!(instance.qid(), 3333);
    }

    #[test]
    fn test_threshold_map() {
        let values = vec![5.0, 7.0, 3.0, 2.0, 1.0, 8.0, 9.0, 4.0, 6.0];

        let map = ThresholdMap::new(values, 3);

        assert_eq!(
            map.thresholds,
            vec![
                1.0 + 0.0 * 8.0 / 3.0, // 1.0
                1.0 + 1.0 * 8.0 / 3.0, // 3.66
                1.0 + 2.0 * 8.0 / 3.0, // 6.33
                std::f64::MAX,
            ]
        );

        assert_eq!(map.map, vec![2, 3, 1, 1, 0, 3, 3, 2, 2]);
    }

    #[test]
    fn test_data_set_lambda_weight() {
        // (label, qid, feature_values)
        let data = vec![
            (3.0, 1, vec![5.0]),
            (2.0, 1, vec![7.0]),
            (3.0, 1, vec![3.0]),
            (1.0, 1, vec![2.0]),
            (0.0, 1, vec![1.0]),
            (2.0, 1, vec![8.0]),
            (4.0, 1, vec![9.0]),
            (1.0, 1, vec![4.0]),
            (0.0, 1, vec![6.0]),
        ];

        let dataset: DataSet = data.into_iter().collect();

        let mut training = TrainingSet::new(&dataset, 3);
        training.update_lambdas_weights(&metric::new("NDCG", 10).unwrap());

        // The values are verified by hand. This test is kept as a
        // guard for future modifications.
        assert_eq!(
            training.lambdas,
            &[
                0.2959880583703105,
                -0.05406635038708441,
                0.06664831928002701,
                -0.10688704271796713,
                -0.1309783051272036,
                -0.056352467003334426,
                0.2573545140200802,
                -0.11687432957979353,
                -0.15483239685503464,
            ]
        );
        assert_eq!(
            training.weights,
            &[
                0.2503273430028968,
                0.07986338018045583,
                0.05890748809444887,
                0.056771982359676655,
                0.0654891525636018,
                0.037537655576830996,
                0.1286772570100401,
                0.06008388967286634,
                0.07741619842751732,
            ]
        );
    }

    #[test]
    fn test_data_set_sample_split() {
        // (label, qid, feature_values)
        let data = vec![         // lambda values to fit in the first iteration.
            (3.0, 1, vec![5.0]), // 0.2959880583703105,
            (2.0, 1, vec![7.0]), // -0.05406635038708441,
            (3.0, 1, vec![3.0]), // 0.06664831928002701,
            (1.0, 1, vec![2.0]), // -0.10688704271796713,
            (0.0, 1, vec![1.0]), // -0.1309783051272036,
            (2.0, 1, vec![8.0]), // -0.056352467003334426,
            (4.0, 1, vec![9.0]), // 0.2573545140200802,
            (1.0, 1, vec![4.0]), // -0.11687432957979353,
            (0.0, 1, vec![6.0]), // -0.15483239685503464,
        ];

        let dataset: DataSet = data.into_iter().collect();

        let mut training = TrainingSet::new(&dataset, 3);
        training.update_lambdas_weights(&metric::new("NDCG", 10).unwrap());

        let sample = TrainingSample::from(&training);
        let (fid, threshold, _s, _left, _right) = sample.split(1).unwrap();
        assert_eq!(fid, 1);
        assert_eq!(threshold, 1.0);
    }

    #[test]
    fn test_data_set_sample_non_split() {
        // (label, qid, feature_values)
        let data = vec![
            (3.0, 1, vec![5.0]), // 0
            (2.0, 1, vec![7.0]), // 1
            (3.0, 1, vec![3.0]), // 2
            (1.0, 1, vec![2.0]), // 3
            (0.0, 1, vec![1.0]), // 4
            (2.0, 1, vec![8.0]), // 5
            (4.0, 1, vec![9.0]), // 6
            (1.0, 1, vec![4.0]), // 7
            (0.0, 1, vec![6.0]), // 8
        ];

        let dataset: DataSet = data.into_iter().collect();

        // possible splits of feature values:
        // 1 | 2 3 4 5 6 7 8 9
        // 1 2 3 | 4 5 6 7 8 9
        // 1 2 3 4 5 6 | 7 8 9
        let mut training = TrainingSet::new(&dataset, 3);
        training.update_lambdas_weights(&metric::new("NDCG", 10).unwrap());

        let sample = TrainingSample::from(&training);
        assert!(sample.split(9).is_none());
        assert!(sample.split(4).is_none());
        let (fid, threshold, _s, left, _right) = sample.split(3).unwrap();
        assert_eq!(fid, 1);
        assert_eq!(threshold, 3.0 + 2.0 / 3.0);

        assert!(left.split(2).is_none());
    }

    #[bench]
    fn bench_split(b: &mut Bencher) {
        let path = "./data/train-lite.txt";
        let f = std::fs::File::open(path).unwrap();
        let dataset = DataSet::load(f).unwrap();

        let mut training = TrainingSet::new(&dataset, 256);
        training.update_lambdas_weights(&metric::new("NDCG", 10).unwrap());

        let sample = TrainingSample::from(&training);
        b.iter(|| sample.split(1).unwrap());
    }
}
