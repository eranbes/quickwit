// Copyright (C) 2021 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use std::collections::HashSet;
use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;

use tantivy::collector::{Collector, SegmentCollector};
use tantivy::fastfield::{DynamicFastFieldReader, FastFieldReader, FastValue};
use tantivy::schema::{Field, Type};
use tantivy::{DocId, Score, SegmentOrdinal, SegmentReader, TantivyError};

use crate::filters::TimestampFilter;
use crate::SearchError;

#[derive(Clone)]
pub struct FastFieldSegmentCollector<Item: FastValue> {
    fast_field_values: Vec<Item>,
    fast_field_reader: DynamicFastFieldReader<Item>,
    timestamp_filter_opt: Option<TimestampFilter>,
}

impl<Item: FastValue> FastFieldSegmentCollector<Item> {
    pub fn new(
        fast_field_reader: DynamicFastFieldReader<Item>,
        timestamp_filter_opt: Option<TimestampFilter>,
    ) -> Self {
        Self {
            fast_field_values: vec![],
            fast_field_reader,
            timestamp_filter_opt,
        }
    }

    fn accept_document(&self, doc_id: DocId) -> bool {
        if let Some(ref timestamp_filter) = self.timestamp_filter_opt {
            return timestamp_filter.is_within_range(doc_id);
        }
        true
    }
}

impl<Item: FastValue> SegmentCollector for FastFieldSegmentCollector<Item> {
    type Fruit = Vec<Item>;

    fn collect(&mut self, doc_id: DocId, _score: Score) {
        if !self.accept_document(doc_id) {
            return;
        }
        let fast_field_value = self.fast_field_reader.get(doc_id);
        self.fast_field_values.push(fast_field_value);
    }

    fn harvest(self) -> Vec<Item> {
        self.fast_field_values
    }
}

#[derive(Clone)]
pub struct FastFieldCollector<Item: FastValue> {
    pub fast_field_to_collect: String,
    pub timestamp_field_opt: Option<Field>,
    pub start_timestamp_opt: Option<i64>,
    pub end_timestamp_opt: Option<i64>,
    _marker: PhantomData<Item>,
}

impl<Item: FastValue> Collector for FastFieldCollector<Item> {
    type Child = FastFieldSegmentCollector<Item>;
    type Fruit = Vec<Item>;

    fn for_segment(
        &self,
        _segment_ord: SegmentOrdinal,
        segment_reader: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let timestamp_filter_opt = if let Some(timestamp_field) = self.timestamp_field_opt {
            TimestampFilter::new(
                timestamp_field,
                self.start_timestamp_opt,
                self.end_timestamp_opt,
                segment_reader,
            )?
        } else {
            None
        };
        let field = segment_reader
            .schema()
            .get_field(&self.fast_field_to_collect)
            .ok_or_else(|| TantivyError::SchemaError("field does not exist".to_owned()))?;
        // TODO: would be nice to access directly to typed_fast_field_reader
        let fast_field_slice = segment_reader.fast_fields().fast_field_data(field, 0)?;
        let fast_field_reader = DynamicFastFieldReader::open(fast_field_slice)?;
        Ok(FastFieldSegmentCollector::new(
            fast_field_reader,
            timestamp_filter_opt,
        ))
    }

    fn requires_scoring(&self) -> bool {
        // We do not need BM25 scoring in Quickwit.
        false
    }

    fn merge_fruits(&self, segment_fruits: Vec<Vec<Item>>) -> tantivy::Result<Self::Fruit> {
        Ok(segment_fruits.into_iter().flatten().collect::<Vec<_>>())
    }
}

#[derive(Clone)]
pub struct FastFieldCollectorBuilder {
    fast_field_value_type: Type,
    fast_field_name: String,
    timestamp_field_name: Option<String>,
    timestamp_field: Option<Field>,
    start_timestamp: Option<i64>,
    end_timestamp: Option<i64>,
}

impl FastFieldCollectorBuilder {
    pub fn new(
        fast_field_value_type: Type,
        fast_field_name: String,
        timestamp_field_name: Option<String>,
        timestamp_field: Option<Field>,
        start_timestamp: Option<i64>,
        end_timestamp: Option<i64>,
    ) -> crate::Result<Self> {
        match fast_field_value_type {
            Type::U64 | Type::I64 => (),
            _ => {
                return Err(SearchError::InvalidQuery(format!(
                    "Fast field type `{:?}` not supported",
                    fast_field_value_type
                )));
            }
        }
        Ok(Self {
            fast_field_value_type,
            fast_field_name,
            timestamp_field_name,
            timestamp_field,
            start_timestamp,
            end_timestamp,
        })
    }

    pub fn value_type(&self) -> Type {
        self.fast_field_value_type
    }

    pub fn fast_field_to_warm(&self) -> HashSet<String> {
        let mut fields = HashSet::new();
        fields.insert(self.fast_field_name.clone());
        if let Some(timestamp_field_name) = &self.timestamp_field_name {
            fields.insert(timestamp_field_name.clone());
        }
        fields
    }

    pub fn typed_build<TFastValue: FastValue>(&self) -> FastFieldCollector<TFastValue> {
        FastFieldCollector::<TFastValue> {
            fast_field_to_collect: self.fast_field_name.clone(),
            timestamp_field_opt: self.timestamp_field,
            start_timestamp_opt: self.start_timestamp,
            end_timestamp_opt: self.end_timestamp,
            _marker: PhantomData,
        }
    }

    pub fn build_i64(&self) -> FastFieldCollector<i64> {
        // TODO: check type
        self.typed_build::<i64>()
    }

    pub fn build_u64(&self) -> FastFieldCollector<u64> {
        // TODO: check type
        self.typed_build::<u64>()
    }
}


#[derive(Clone)]
pub struct PartionnedFastFieldCollectorBuilder {
    fast_field_value_type: Type,
    fast_field_name: String,
    partition_by_fast_field_value_type: Type,
    partition_by_fast_field_name: String,
    timestamp_field_name: Option<String>,
    timestamp_field: Option<Field>,
    start_timestamp: Option<i64>,
    end_timestamp: Option<i64>,
}

impl PartionnedFastFieldCollectorBuilder {
    pub fn new(
        fast_field_value_type: Type,
        fast_field_name: String,
        timestamp_field_name: Option<String>,
        timestamp_field: Option<Field>,
        start_timestamp: Option<i64>,
        end_timestamp: Option<i64>,
        partition_by_fast_field_value_type: Type,
        partition_by_fast_field_name: String,
    ) -> crate::Result<Self> {
        match fast_field_value_type {
            Type::U64 | Type::I64 => (),
            _ => {
                return Err(SearchError::InvalidQuery(format!(
                    "Fast field type `{:?}` not supported",
                    fast_field_value_type
                )));
            }
        }
        Ok(Self {
            fast_field_value_type,
            fast_field_name,
            partition_by_fast_field_value_type,
            partition_by_fast_field_name,
            timestamp_field_name,
            timestamp_field,
            start_timestamp,
            end_timestamp,
        })
    }

    pub fn value_type(&self) -> (Type, Type) {
        (
            self.fast_field_value_type,
            self.partition_by_fast_field_value_type,
        )
    }

    pub fn fast_field_to_warm(&self) -> Vec<String> {
        if let Some(timestamp_field_name) = &self.timestamp_field_name {
            vec![
                timestamp_field_name.clone(),
                self.fast_field_name.clone(),
                self.partition_by_fast_field_name.clone(),
            ]
        } else {
            vec![self.fast_field_name.clone()]
        }
    }

    pub fn typed_build<TFastValue: FastValue, TPartitionValue: FastValue>(
        &self,
    ) -> PartionnedFastFieldCollector<TFastValue, TPartitionValue> {
        PartionnedFastFieldCollector::<TFastValue, TPartitionValue> {
            fast_field_to_collect: self.fast_field_name.clone(),
            partition_by_fast_field: self.partition_by_fast_field_name.clone(),
            timestamp_field_opt: self.timestamp_field,
            start_timestamp_opt: self.start_timestamp,
            end_timestamp_opt: self.end_timestamp,
            _marker: PhantomData,
            _marker2: PhantomData,
        }
    }

    pub fn build_i64_i64(&self) -> PartionnedFastFieldCollector<i64, i64> {
        // TODO: check type
        self.typed_build::<i64, i64>()
    }

    pub fn build_u64_u64(&self) -> PartionnedFastFieldCollector<u64, u64> {
        // TODO: check type
        self.typed_build::<u64, u64>()
    }

    pub fn build_i64_u64(&self) -> PartionnedFastFieldCollector<i64, u64> {
        // TODO: check type
        self.typed_build::<i64, u64>()
    }

    pub fn build_u64_i64(&self) -> PartionnedFastFieldCollector<u64, i64> {
        // TODO: check type
        self.typed_build::<u64, i64>()
    }
}

#[derive(Clone)]
pub struct PartionnedFastFieldCollector<Item: FastValue, PartitionItem: FastValue> {
    pub fast_field_to_collect: String,
    pub partition_by_fast_field: String,
    pub timestamp_field_opt: Option<Field>,
    pub start_timestamp_opt: Option<i64>,
    pub end_timestamp_opt: Option<i64>,
    _marker: PhantomData<Item>,
    _marker2: PhantomData<PartitionItem>,
}

pub struct PartitionValues<Item: FastValue, PartitionItem: FastValue> {
    pub partition_value: PartitionItem,
    pub fast_field_values: Vec<Item>,
}

impl<Item: FastValue, PartitionItem: FastValue + Eq + Hash> Collector
    for PartionnedFastFieldCollector<Item, PartitionItem>
{
    type Child = PartitionnedFastFieldSegmentCollector<Item, PartitionItem>;
    type Fruit = Vec<PartitionValues<Item, PartitionItem>>;

    fn for_segment(
        &self,
        _segment_ord: SegmentOrdinal,
        segment_reader: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let timestamp_filter_opt = if let Some(timestamp_field) = self.timestamp_field_opt {
            TimestampFilter::new(
                timestamp_field,
                self.start_timestamp_opt,
                self.end_timestamp_opt,
                segment_reader,
            )?
        } else {
            None
        };
        let field = segment_reader
            .schema()
            .get_field(&self.fast_field_to_collect)
            .ok_or_else(|| TantivyError::SchemaError("field does not exist".to_owned()))?;
        // TODO: would be nice to access directly to typed_fast_field_reader
        let fast_field_slice = segment_reader.fast_fields().fast_field_data(field, 0)?;
        let fast_field_reader = DynamicFastFieldReader::open(fast_field_slice)?;

        let partition_field = segment_reader
            .schema()
            .get_field(&self.partition_by_fast_field)
            .ok_or_else(|| TantivyError::SchemaError("field does not exist".to_owned()))?;
        let partition_by_fast_field_slice = segment_reader
            .fast_fields()
            .fast_field_data(partition_field, 0)?;

        let partition_by_fast_field_reader =
            DynamicFastFieldReader::open(partition_by_fast_field_slice)?;

        Ok(PartitionnedFastFieldSegmentCollector::new(
            fast_field_reader,
            partition_by_fast_field_reader,
            timestamp_filter_opt,
        ))
    }

    fn requires_scoring(&self) -> bool {
        // We do not need BM25 scoring in Quickwit.
        false
    }

    fn merge_fruits(
        &self,
        segment_fruits: Vec<std::collections::HashMap<PartitionItem, Vec<Item>>>,
    ) -> tantivy::Result<Self::Fruit> {
        Ok(segment_fruits
            .into_iter()
            .flat_map(|e| e.into_iter())
            .map(|(partition_value, values)| PartitionValues {
                partition_value,
                fast_field_values: values,
            })
            .collect())
    }
}

#[derive(Clone)]
pub struct PartitionnedFastFieldSegmentCollector<
    Item: FastValue,
    PartitionItem: FastValue + Eq + Hash,
> {
    fast_field_values: std::collections::HashMap<PartitionItem, Vec<Item>>,
    fast_field_reader: DynamicFastFieldReader<Item>,
    partition_by_fast_field_reader: DynamicFastFieldReader<PartitionItem>,
    timestamp_filter_opt: Option<TimestampFilter>,
}

impl<Item: FastValue, PartitionItem: FastValue + Eq + Hash>
    PartitionnedFastFieldSegmentCollector<Item, PartitionItem>
{
    pub fn new(
        fast_field_reader: DynamicFastFieldReader<Item>,
        partition_by_fast_field_reader: DynamicFastFieldReader<PartitionItem>,
        timestamp_filter_opt: Option<TimestampFilter>,
    ) -> Self {
        Self {
            fast_field_values: std::collections::HashMap::default(),
            fast_field_reader,
            partition_by_fast_field_reader,
            timestamp_filter_opt,
        }
    }

    fn accept_document(&self, doc_id: DocId) -> bool {
        if let Some(ref timestamp_filter) = self.timestamp_filter_opt {
            return timestamp_filter.is_within_range(doc_id);
        }
        true
    }
}

impl<Item: FastValue, PartitionItem: FastValue + Hash + Eq> SegmentCollector
    for PartitionnedFastFieldSegmentCollector<Item, PartitionItem>
{
    type Fruit = HashMap<PartitionItem, Vec<Item>>;

    fn collect(&mut self, doc_id: DocId, _score: Score) {
        if !self.accept_document(doc_id) {
            return;
        }
        let fast_field_value = self.fast_field_reader.get(doc_id);
        let fast_field_partition = &self.partition_by_fast_field_reader.get(doc_id);
        if let Some(values) = self.fast_field_values.get_mut(fast_field_partition) {
            values.push(fast_field_value);
        } else {
            self.fast_field_values
                .insert(*fast_field_partition, vec![fast_field_value]);
        }
    }

    fn harvest(self) -> Self::Fruit {
        self.fast_field_values
    }
}

#[cfg(test)]
mod tests {
    use std::iter::FromIterator;

    use super::*;

    #[test]
    fn test_fast_field_collector_builder() -> anyhow::Result<()> {
        let builder = FastFieldCollectorBuilder::new(
            Type::U64,
            "field_name".to_string(),
            Some("field_name".to_string()),
            None,
            None,
            None,
        )?;
        assert_eq!(
            builder.fast_field_to_warm(),
            HashSet::from_iter(["field_name".to_string()])
        );
        let builder = FastFieldCollectorBuilder::new(
            Type::U64,
            "field_name".to_string(),
            Some("timestamp_field_name".to_string()),
            None,
            None,
            None,
        )?;
        assert_eq!(
            builder.fast_field_to_warm(),
            HashSet::from_iter(["field_name".to_string(), "timestamp_field_name".to_string()])
        );
        Ok(())
    }
}
