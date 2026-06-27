CREATE FUNCTION vpic.felementattributevalue(elementid integer, attributeid character varying) RETURNS character varying
    LANGUAGE plpgsql
    AS $$
declare
	v varchar(2000) = AttributeId;
begin
	CASE ElementId
        WHEN 2 THEN
			select name from vpic.BatteryType where cast(Id as character varying) = AttributeId into v;
			return v;
		
        WHEN 3 THEN
			select name from vpic.BedType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 4 THEN
			select name from vpic.BodyCab where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 5 THEN
			select name from vpic.BodyStyle where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 10 THEN
			select name from vpic.DestinationMarket where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 15 THEN
			select name from vpic.DriveType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 23 THEN
			select name from vpic.EntertainmentSystem where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 24 THEN
			select name from vpic.FuelType where cast(Id as character varying) = AttributeId into v;
			return v;
			
		WHEN 25 THEN
			select name from vpic.GrossVehicleWeightRating where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 26 THEN
			select name from vpic.Make where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 27 THEN
			select name from vpic.Manufacturer where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 28 THEN
			select name from vpic.Model where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 36 THEN
			select name from vpic.Steering where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 37 THEN
			select name from vpic.Transmission where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 39 THEN
			select name from vpic.VehicleType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 42 THEN
			select name from vpic.BrakeSystem where cast(Id as character varying) = AttributeId into v;
			return v;
			
		WHEN 55 THEN
			select name from vpic.AirBagLocations where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 56 THEN
			select name from vpic.AirBagLocations where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 60 THEN
			select name from vpic.WheelBaseType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 62 THEN
			select name from vpic.ValvetrainDesign where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 64 THEN
			select name from vpic.EngineConfiguration where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 65 THEN
			select name from vpic.AirBagLocFront where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 66 THEN
			select name from vpic.FuelType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 67 THEN
			select name from vpic.FuelDeliveryType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 69 THEN
			select name from vpic.AirBagLocKnee where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 72 THEN
			select name from vpic.EVDriveUnit where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 75 THEN
			select name from vpic.Country where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 78 THEN
			select name from vpic.Pretensioner where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 79 THEN
			select name from vpic.SeatBeltsAll where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 81 THEN
			select name from vpic.AdaptiveCruiseControl where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 86 THEN
			select name from vpic.ABS where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 87 THEN
			select name from vpic.AutoBrake where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 88 THEN
			select name from vpic.BlindSpotMonitoring where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 96 THEN
			select name from vpic.vNCSABodyType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 97 THEN
			select name from vpic.vNCSAMake where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 98 THEN
			select name from vpic.vNCSAModel where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 99 THEN
			select name from vpic.ECS where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 100 THEN
			select name from vpic.TractionControl where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 101 THEN
			select name from vpic.ForwardCollisionWarning where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 102 THEN
			select name from vpic.LaneDepartureWarning where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 103 THEN
			select name from vpic.LaneKeepSystem where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 104 THEN
			select name from vpic.RearVisibilityCamera where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 105 THEN
			select name from vpic.ParkAssist where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 107 THEN
			select name from vpic.AirBagLocations where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 116 THEN
			select name from vpic.TrailerType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 117 THEN
			select name from vpic.TrailerBodyType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 122 THEN
			select name from vpic.CoolingType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 126 THEN
			select name from vpic.ElectrificationLevel where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 127 THEN
			select name from vpic.ChargerLevel where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 135 THEN
			select name from vpic.Turbo where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 143 THEN
			select name from vpic.ErrorCode where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 145 THEN
			select name from vpic.AxleConfiguration where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 148 THEN
			select name from vpic.BusFloorConfigType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 149 THEN
			select name from vpic.BusType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 151 THEN
			select name from vpic.CustomMotorcycleType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 152 THEN
			select name from vpic.MotorcycleSuspensionType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 153 THEN
			select name from vpic.MotorcycleChassisType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 168 THEN
			select name from vpic.TPMS where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 170 THEN
			select name from vpic.DynamicBrakeSupport where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 171 THEN
			select name from vpic.PedestrianAutomaticEmergencyBraking where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 172 THEN
			select name from vpic.AutoReverseSystem where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 173 THEN
			select name from vpic.AutomaticPedestrainAlertingSound where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 174 THEN
			select name from vpic.CAN_AACN where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 175 THEN
			select name from vpic.EDR where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 176 THEN
			select name from vpic.KeylessIgnition where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 177 THEN
			select name from vpic.DaytimeRunningLight where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 178 THEN
			select name from vpic.LowerBeamHeadlampLightSource where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 179 THEN
			select name from vpic.SemiautomaticHeadlampBeamSwitching where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 180 THEN
			select name from vpic.AdaptiveDrivingBeam where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 183 THEN
			select name from vpic.RearCrossTrafficAlert where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 184 THEN
			select name from vpic.GrossVehicleWeightRating where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 185 THEN
			select name from vpic.GrossVehicleWeightRating where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 190 THEN
			select name from vpic.GrossVehicleWeightRating where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 192 THEN
			select name from vpic.RearAutomaticEmergencyBraking where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 193 THEN
			select name from vpic.BlindSpotIntervention where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 194 THEN
			select name from vpic.LaneCenteringAssistance where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 195 THEN
			select name from vpic.NonLandUse where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 200 THEN
			select name from vpic.FuelTankType where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 201 THEN
			select name from vpic.FuelTankMaterial where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 202 THEN
			select name from vpic.CombinedBrakingSystem where cast(Id as character varying) = AttributeId into v;
			return v;

		WHEN 203 THEN
			select name from vpic.WheelieMitigation where cast(Id as character varying) = AttributeId into v;
			return v;
	ELSE
		return v;
    END CASE;

	return v;
end;
$$;
